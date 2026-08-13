use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
use phoenix_fork_sandbox::{CounterfactualResult, SimulationStatus, UnsignedTransactionPlan};
use phoenix_live_executor::activation_request::{
    canonical_domain_hash, write_atomic_request, ActivationCandidateEvidence, ActivationRequest,
    ACTIVATION_REQUEST_SCHEMA, ACTIVATION_REQUEST_TTL_SECONDS,
};
use phoenix_live_executor::control_environment::{
    control_address_environment_with, required_environment_with, MissingEnvironment,
};
use phoenix_live_executor::economic_control::{
    activate_canary, evaluate_promotion, AutomationAuthorization, EconomicPhase, EvidenceGate,
    PromotionEvidence, ReadinessBinding, SizeLevel, Transition, MAXIMUM_REVIEWED_INPUT_WEI,
};
use phoenix_live_executor::model::{CanonicalAddress, ExecutionRequest, ValidatedLeg};
use phoenix_live_executor::owner_bootstrap::{
    configured_preflight_from_environment, execute_from_environment, owner_plan_from_environment,
    runtime_preflight_from_environment, OwnerBootstrapError, OwnerMutation,
};
use phoenix_live_executor::rpc::{ExecutionRpc, HttpExecutionRpc};
use phoenix_live_executor::store::fail_close_execution_authority;
#[cfg(test)]
use phoenix_live_executor::REVERSE_ROUTE_FINGERPRINT;
use phoenix_live_executor::{
    reviewed_route_policies, reviewed_route_policy, APPROVAL_POLICY_VERSION,
    CURRENT_ROUTE_FINGERPRINT, REQUEST_SCHEMA_VERSION,
};
use rpc_gateway::hunter_state::{HunterStateResponse, HUNTER_STATE_RESPONSE_SCHEMA};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use url::Url;
use uuid::Uuid;

const POLICY: &str = include_str!("../../config/phoenix-route-policy-v1.json");
const UNIVERSE: &str = include_str!("../../config/phoenix-route-universe-v1.json");
const MIGRATIONS: [(&str, &str); 9] = [
    (
        "phoenix.live-canary-schema.v1",
        include_str!("../schema/001_live_canary.sql"),
    ),
    (
        "phoenix.live-canary-schema.v2",
        include_str!("../schema/002_approval_evidence.sql"),
    ),
    (
        "phoenix.live-canary-schema.v3",
        include_str!("../schema/003_autonomous_hunter_contracts.sql"),
    ),
    (
        "phoenix.live-canary-schema.v4",
        include_str!("../schema/004_autonomous_live_runtime.sql"),
    ),
    (
        "phoenix.live-canary-schema.v5",
        include_str!("../schema/005_closed_loop_economic_control.sql"),
    ),
    (
        "phoenix.live-canary-schema.v6",
        include_str!("../schema/006_atlas_aave_revenue_lanes.sql"),
    ),
    (
        "phoenix.live-canary-schema.v7",
        include_str!("../schema/007_aave_economic_diagnostics.sql"),
    ),
    (
        "phoenix.live-canary-schema.v8",
        include_str!("../schema/008_revenue_provider_authority.sql"),
    ),
    (
        "phoenix.live-canary-schema.v9",
        include_str!("../schema/009_single_primary_provider_authority.sql"),
    ),
];
const ACTIVATE_ACK: &str = "ACTIVATE_READY_MIN_CANARY_42161";
const ARM_REVENUE_LANES_ACK: &str = "ARM_ATLAS_AAVE_LIVE_42161";
const SET_REVENUE_SIZE_MAX_REVIEWED_ACK: &str = "SET_MAX_REVIEWED_LIVE_SIZE_42161";
const DISARM_ACK: &str = "DISARM_AUTONOMOUS_LIVE_42161";
const DISARMED_DEPLOY_ACK: &str = "INSTALL_DISARMED_EVIDENCE_RELEASE_42161";
const EVIDENCE_START_ACK: &str = "START_DISARMED_EVIDENCE_42161";
const READINESS_ACK: &str = "CREATE_HASH_BOUND_CANARY_READINESS_42161";
const AUTHORIZATION_ACK: &str = "INSTALL_BOUNDED_AUTOMATION_AUTHORIZATION_42161";
const MATERIALIZE_ACTIVATION_ACK: &str = "MATERIALIZE_VALIDATED_MIN_CANARY_42161";
const ACTIVATION_REQUEST_OUTBOX_ENV: &str = "PHOENIX_ACTIVATION_REQUEST_OUTBOX";
const MAX_CONTROL_FILE_BYTES: u64 = 256 * 1024;
const MAX_READINESS_RESPONSE_BYTES: u64 = 64 * 1024;
const RPC_GATEWAY_READINESS_URL: &str = "http://rpc-gateway:9300/readyz";
const ATLAS_HUNTER_READINESS_URL: &str = "http://atlas-observer:9700/readyz";
const REVENUE_PROVIDER_AUTHORITY_CHECK_INTERVAL: Duration = Duration::from_secs(10);
const REVENUE_PROVIDER_FAILURE_MINIMUM_DURATION: i64 = 5 * 60 * 1000;
const IMAGE_RUNTIME_PROBE_COMMAND: &str = "__image_runtime_probe__";
const IMAGE_RUNTIME_OK: &str = "AUTONOMOUS_CONTROL_RUNTIME_OK";

fn reviewed_policy_values() -> ControlResult<Vec<Value>> {
    let mut policies = Vec::new();
    let mut fingerprints = std::collections::BTreeSet::new();
    for contract in reviewed_route_policies() {
        let policy: Value =
            serde_json::from_str(contract).map_err(|_| "route policy is invalid")?;
        verify_hash(
            &policy,
            "policy_hash",
            "route-policy",
            "phoenix.route-policy.v1",
        )?;
        let fingerprint = value_text(&policy, "route_fingerprint")?;
        if !fingerprints.insert(fingerprint.to_string()) {
            return Err("reviewed route fingerprint is duplicated".into());
        }
        policies.push(policy);
    }
    Ok(policies)
}

fn reviewed_policy_value(fingerprint: &str) -> ControlResult<Value> {
    let contract =
        reviewed_route_policy(fingerprint).ok_or("route is outside the reviewed universe")?;
    let policy: Value = serde_json::from_str(contract).map_err(|_| "route policy is invalid")?;
    verify_hash(
        &policy,
        "policy_hash",
        "route-policy",
        "phoenix.route-policy.v1",
    )?;
    Ok(policy)
}

#[derive(Debug)]
enum ControlError {
    Message(&'static str),
    MissingEnvironment(MissingEnvironment),
    OwnerBootstrap(OwnerBootstrapError),
}

impl Display for ControlError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Message(message) => formatter.write_str(message),
            Self::MissingEnvironment(error) => Display::fmt(error, formatter),
            Self::OwnerBootstrap(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for ControlError {}

impl From<&'static str> for ControlError {
    fn from(message: &'static str) -> Self {
        Self::Message(message)
    }
}

impl From<MissingEnvironment> for ControlError {
    fn from(error: MissingEnvironment) -> Self {
        Self::MissingEnvironment(error)
    }
}

impl From<OwnerBootstrapError> for ControlError {
    fn from(error: OwnerBootstrapError) -> Self {
        Self::OwnerBootstrap(error)
    }
}

type ControlResult<T> = Result<T, ControlError>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    if let Err(error) = run().await {
        eprintln!("AUTONOMOUS_CONTROL_FAILED: {error}");
        return Err(io::Error::other("autonomous control failed").into());
    }
    Ok(())
}

async fn run() -> ControlResult<()> {
    let command = env::args().nth(1).ok_or("command is required")?;
    match command.as_str() {
        IMAGE_RUNTIME_PROBE_COMMAND => println!("{IMAGE_RUNTIME_OK}"),
        "preflight" => preflight().await?,
        "owner-plan" => owner_plan().await?,
        "owner-configure" => owner_mutation(OwnerMutation::Configure).await?,
        "owner-configured-preflight" => owner_configured_preflight().await?,
        "owner-unpause" => owner_mutation(OwnerMutation::Unpause).await?,
        "owner-pause" => owner_mutation(OwnerMutation::Pause).await?,
        "migrate" => migrate(&database_pool().await?).await?,
        "disarmed-deploy" => disarmed_deploy(&database_pool().await?).await?,
        "evidence-start" => start_evidence().await?,
        "create-readiness" => create_readiness(&database_pool().await?).await?,
        "install-authorization" => install_authorization(&database_pool().await?).await?,
        "materialize-activation-contracts" => materialize_activation_contracts().await?,
        "activate-ready-canary" => activate(&database_pool().await?).await?,
        "set-revenue-size-max-reviewed" => set_revenue_size_max_reviewed().await?,
        "arm-revenue-lanes" => arm_revenue_lanes().await?,
        "activate" => return Err("direct activation is disabled; use activate-ready-canary".into()),
        "evaluate-economic-control" => evaluate_economic_control(&database_pool().await?).await?,
        "supervise-economic-control" => supervise_economic_control().await?,
        "disarm" => disarm(&database_pool().await?).await?,
        "status" => status(&database_pool().await?).await?,
        "reconciliation-status" => reconciliation_status(&database_pool().await?).await?,
        _ => return Err("unsupported command".into()),
    }
    Ok(())
}

async fn database_pool() -> ControlResult<PgPool> {
    let dsn = required("POSTGRES_DSN")?;
    PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&dsn)
        .await
        .map_err(|_| "database connection failed".into())
}

async fn preflight() -> ControlResult<()> {
    let addresses = control_address_environment_with(|name| env::var(name).ok())?;
    let wallet = CanonicalAddress::parse(&addresses.wallet_address)
        .map_err(|_| "wallet address is invalid")?;
    let executor = CanonicalAddress::parse(&addresses.executor_address)
        .map_err(|_| "executor address is invalid")?;
    let primary_url =
        Url::parse(&required("PRODUCTION_RPC_URL")?).map_err(|_| "primary RPC URL is invalid")?;
    let allowlist = required("LIVE_EXECUTOR_RPC_ALLOWLIST")?
        .split(',')
        .map(|value| Url::parse(value).map_err(|_| "RPC allowlist is invalid"))
        .collect::<Result<Vec<_>, _>>()?;
    let primary = HttpExecutionRpc::new_production_authenticated(
        primary_url,
        &allowlist,
        &required("LIVE_EXECUTOR_RPC_HEADER_NAME")?,
        &required("LIVE_EXECUTOR_RPC_HEADER_FILE")?,
    )
    .map_err(|_| "primary RPC is not allowlisted")?;
    if primary
        .chain_id()
        .await
        .map_err(|_| "primary chain identity is unavailable")?
        != 42_161
    {
        return Err("RPC chain identity mismatch".into());
    }
    let expected_owner = CanonicalAddress::parse(&required("LIVE_EXECUTOR_EXPECTED_OWNER")?)
        .map_err(|_| "expected owner is invalid")?;
    let expected_flash_provider =
        CanonicalAddress::parse(&required("LIVE_EXECUTOR_EXPECTED_FLASH_PROVIDER")?)
            .map_err(|_| "expected flash provider is invalid")?;
    let expected_code_hash = required("LIVE_EXECUTOR_EXECUTOR_CODE_HASH")?;
    let maximum_input = required_u128("LIVE_EXECUTOR_MAX_INPUT_AMOUNT")?;
    if primary
        .wallet_balance(wallet)
        .await
        .map_err(|_| "wallet balance is unavailable")?
        == 0
    {
        return Err("wallet has no native gas balance".into());
    }
    let (owner, flash_provider) = primary
        .executor_owner_and_flash_provider(executor)
        .await
        .map_err(|_| "executor ownership state is unavailable")?;
    if owner != expected_owner || flash_provider != expected_flash_provider {
        return Err("executor owner or flash provider mismatch".into());
    }

    let policy: Value = serde_json::from_str(POLICY).map_err(|_| "route policy is invalid")?;
    verify_hash(
        &policy,
        "policy_hash",
        "route-policy",
        "phoenix.route-policy.v1",
    )?;
    let token_path = policy
        .get("token_path")
        .and_then(Value::as_array)
        .ok_or("route token path is invalid")?
        .iter()
        .map(|value| {
            CanonicalAddress::parse(value.as_str().ok_or("route token path is invalid")?)
                .map_err(|_| "route token path is invalid")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let pools = policy
        .get("pool_addresses")
        .and_then(Value::as_array)
        .ok_or("route pools are invalid")?;
    let factories = policy
        .get("factory_addresses")
        .and_then(Value::as_array)
        .ok_or("route factories are invalid")?;
    let fees = policy
        .get("fees")
        .and_then(Value::as_array)
        .ok_or("route fees are invalid")?;
    let directions = policy
        .get("directions")
        .and_then(Value::as_array)
        .ok_or("route directions are invalid")?;
    if pools.len() + 1 != token_path.len()
        || factories.len() != pools.len()
        || fees.len() != pools.len()
        || directions.len() != pools.len()
    {
        return Err("route path is inconsistent".into());
    }
    let legs = (0..pools.len())
        .map(|index| {
            Ok(ValidatedLeg {
                pool: CanonicalAddress::parse(
                    pools[index].as_str().ok_or("route pool is invalid")?,
                )
                .map_err(|_| "route pool is invalid")?,
                factory: Some(
                    CanonicalAddress::parse(
                        factories[index]
                            .as_str()
                            .ok_or("route factory is invalid")?,
                    )
                    .map_err(|_| "route factory is invalid")?,
                ),
                token_in: token_path[index],
                token_out: token_path[index + 1],
                fee: fees[index]
                    .as_u64()
                    .and_then(|value| value.try_into().ok())
                    .ok_or("route fee is invalid")?,
                zero_for_one: directions[index].as_str() == Some("zero_for_one"),
                min_amount_out: 1,
            })
        })
        .collect::<Result<Vec<_>, &'static str>>()?;
    let routers = required("ENGINE_ROUTER_ADDRESSES")?
        .split(',')
        .map(|value| {
            CanonicalAddress::parse(value.trim()).map_err(|_| "reviewed router is invalid")
        })
        .collect::<Result<Vec<_>, _>>()?;
    if routers.is_empty() || routers.len() > 3 {
        return Err("reviewed router set is invalid".into());
    }
    for router in routers {
        let request = preflight_request(
            executor,
            router,
            maximum_input,
            token_path.clone(),
            legs.clone(),
        )?;
        if !primary
            .execution_contract_ready(&request, wallet, &expected_code_hash)
            .await
            .map_err(|_| "executor configuration state is unavailable")?
        {
            return Err("executor configuration is not LIVE-ready".into());
        }
    }
    println!(
        "AUTONOMOUS_PREFLIGHT_OK: chain=42161 wallet_gas=positive executor_state=ready providers=2"
    );
    Ok(())
}

async fn owner_plan() -> ControlResult<()> {
    let payload = owner_plan_from_environment().await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).map_err(|_| "owner plan serialization failed")?
    );
    Ok(())
}

async fn owner_configured_preflight() -> ControlResult<()> {
    let evidence = configured_preflight_from_environment().await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&evidence)
            .map_err(|_| "owner preflight evidence serialization failed")?
    );
    println!("EXECUTOR_OWNER_CONFIGURED_PREFLIGHT_OK");
    Ok(())
}

async fn owner_mutation(mutation: OwnerMutation) -> ControlResult<()> {
    let evidence = execute_from_environment(mutation).await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&evidence)
            .map_err(|_| "owner mutation evidence serialization failed")?
    );
    let marker = evidence
        .get("status")
        .and_then(Value::as_str)
        .ok_or("owner mutation evidence is invalid")?;
    if mutation == OwnerMutation::Unpause {
        preflight().await?;
    }
    println!("{marker}");
    Ok(())
}

fn preflight_request(
    executor: CanonicalAddress,
    router: CanonicalAddress,
    maximum_input: u128,
    token_path: Vec<CanonicalAddress>,
    legs: Vec<ValidatedLeg>,
) -> ControlResult<ExecutionRequest> {
    let flash_asset = *token_path.first().ok_or("route token path is empty")?;
    let now = Utc::now();
    Ok(ExecutionRequest {
        id: Uuid::nil(),
        opportunity_id: Uuid::nil(),
        schema_version: REQUEST_SCHEMA_VERSION.to_string(),
        chain_id: 42_161,
        route_id: [0; 32],
        route_fingerprint: CURRENT_ROUTE_FINGERPRINT.to_string(),
        route_type: phoenix_live_executor::model::ExecutionRouteType::PhoenixDexV1,
        aave_liquidation: None,
        selected_size: maximum_input,
        token_path,
        origin_router: router,
        executor_address: executor,
        executor_code_hash: required("LIVE_EXECUTOR_EXECUTOR_CODE_HASH")?,
        calldata_hash: "0".repeat(64),
        simulation_result_hash: "0".repeat(64),
        plan_hash: "0".repeat(64),
        pinned_block_number: 1,
        pinned_block_hash: format!("0x{}", "0".repeat(64)),
        flash_asset,
        flash_amount: maximum_input,
        maximum_input_amount: maximum_input,
        minimum_profit: 1,
        expected_profit: 1,
        deadline: now + chrono::Duration::minutes(1),
        legs,
        gas_limit: 1,
        max_fee_per_gas: 1,
        max_priority_fee_per_gas: 1,
        approved_by: "autonomous_policy".to_string(),
        approved_at: now,
        approval_deadline: now + chrono::Duration::minutes(1),
        policy_version: APPROVAL_POLICY_VERSION.to_string(),
        approval_digest: "0".repeat(64),
    })
}

async fn migrate(pool: &PgPool) -> Result<(), &'static str> {
    for (version, sql) in MIGRATIONS {
        let schema_exists: bool =
            sqlx::query_scalar("SELECT to_regclass('live_canary.schema_contract') IS NOT NULL")
                .fetch_one(pool)
                .await
                .map_err(|_| "schema inspection failed")?;
        let installed = if schema_exists {
            sqlx::query_scalar(
                "SELECT EXISTS(
                     SELECT 1 FROM live_canary.schema_contract WHERE version = $1
                 )",
            )
            .bind(version)
            .fetch_one(pool)
            .await
            .map_err(|_| "schema inspection failed")?
        } else {
            false
        };
        if !installed {
            sqlx::raw_sql(sql)
                .execute(pool)
                .await
                .map_err(|_| "migration failed")?;
        }
    }
    require_schema(pool).await?;
    println!("AUTONOMOUS_MIGRATION_OK: phoenix.live-canary-schema.v9");
    Ok(())
}

#[derive(Debug, Deserialize, Serialize)]
struct ReadinessFile {
    schema_version: String,
    readiness_id: Uuid,
    binding: ReadinessBinding,
    evidence: EvidenceGate,
    readiness_hash: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct AuthorizationFile {
    schema_version: String,
    authorization_id: Uuid,
    authorization: AutomationAuthorization,
    authorization_hash: String,
}

#[derive(Debug, Serialize)]
struct ActivationMaterialization {
    schema_version: &'static str,
    request_id: Uuid,
    request_hash: String,
    readiness: Value,
    authorization: Value,
}

#[derive(Clone, Copy, Debug)]
struct EvidenceAuthorityState {
    legacy_armed: bool,
    legacy_kill_switch: bool,
    global_armed: bool,
    global_kill_switch: bool,
    global_disarmed: bool,
    route_enabled: bool,
    route_kill_switch: bool,
    active_attempts: i64,
    unresolved_receipts: i64,
}

async fn disarmed_deploy(pool: &PgPool) -> ControlResult<()> {
    if required("PHOENIX_DISARMED_DEPLOY_ACK")? != DISARMED_DEPLOY_ACK {
        return Err("disarmed deployment acknowledgement is invalid".into());
    }
    require_schema(pool).await?;
    let release_sha = required("PHOENIX_RELEASE_SHA")?;
    if !canonical_hex(&release_sha, 40) {
        return Err("release SHA is invalid".into());
    }
    let engine_image_digest = image_digest(&required("PHOENIX_ENGINE_IMAGE")?)?;
    let executor_code_hash = required("LIVE_EXECUTOR_EXECUTOR_CODE_HASH")?;
    if !canonical_hex(&executor_code_hash, 64) {
        return Err("executor code hash is invalid".into());
    }
    let daily_loss_limit = required_u128("LIVE_EXECUTOR_MAX_DAILY_LOSS_WEI")?;
    let policies = reviewed_policy_values()?;
    let policy = policies
        .iter()
        .find(|policy| {
            policy.get("route_fingerprint").and_then(Value::as_str)
                == Some(CURRENT_ROUTE_FINGERPRINT)
        })
        .ok_or("default reviewed route policy is missing")?;
    let universe: Value =
        serde_json::from_str(UNIVERSE).map_err(|_| "route universe is invalid")?;
    verify_hash(
        &universe,
        "universe_hash",
        "route-universe",
        "phoenix.route-universe.v1",
    )?;
    let route_fingerprint = value_text(policy, "route_fingerprint")?;
    let route_policy_hash = value_text(policy, "policy_hash")?;
    let route_universe_hash = value_text(&universe, "universe_hash")?;
    for reviewed_policy in &policies {
        if value_text(reviewed_policy, "route_universe_hash")? != route_universe_hash
            || value_u128(reviewed_policy, "maximum_input_amount")? != MAXIMUM_REVIEWED_INPUT_WEI
            || value_u128(reviewed_policy, "minimum_input_amount")? != SizeLevel::Min.amount_wei()
        {
            return Err("reviewed route policy does not match the economic ladder".into());
        }
    }

    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| "database transaction failed")?;
    let previous = economic_state_for_update(&mut transaction).await?;
    let global_epoch: i64 = sqlx::query_scalar(
        "UPDATE live_canary.autonomous_global_control
         SET armed = false, kill_switch = true, execution_mode = 'disarmed',
             maximum_input_amount = $1::numeric, daily_loss_limit = $2::numeric,
             daily_ordering_budget = 0, maximum_concurrent_candidates = 1,
             control_epoch = control_epoch + 1,
             disarm_reason = 'disarmed_deploy', control_hash = NULL,
             control_contract = NULL, updated_at = now()
         WHERE singleton
         RETURNING control_epoch",
    )
    .bind(MAXIMUM_REVIEWED_INPUT_WEI.to_string())
    .bind(daily_loss_limit.to_string())
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "global disarmed deployment failed")?;
    sqlx::query(
        "UPDATE live_canary.control
         SET armed = false, kill_switch = true,
             disarm_reason = 'disarmed_deploy', updated_at = now()
         WHERE singleton",
    )
    .execute(&mut *transaction)
    .await
    .map_err(|_| "legacy disarmed deployment failed")?;
    let mut route_epoch = None;
    for reviewed_policy in &policies {
        let fingerprint = value_text(reviewed_policy, "route_fingerprint")?;
        let policy_hash = value_text(reviewed_policy, "policy_hash")?;
        let epoch: i64 = sqlx::query_scalar(
            "INSERT INTO live_canary.autonomous_route_controls(
                route_fingerprint, route_policy_hash, enabled, kill_switch,
                current_size_level, maximum_permitted_size, cooldown_until,
                control_epoch, disarm_reason, control_hash, control_contract, updated_at
             ) VALUES (
                $1, $2, false, true, 'MIN', $3::numeric, NULL,
                0, 'disarmed_deploy', NULL, NULL, now()
             )
             ON CONFLICT (route_fingerprint) DO UPDATE SET
                route_policy_hash = EXCLUDED.route_policy_hash,
                enabled = false,
                kill_switch = true,
                current_size_level = 'MIN',
                maximum_permitted_size = EXCLUDED.maximum_permitted_size,
                cooldown_until = NULL,
                control_epoch = live_canary.autonomous_route_controls.control_epoch + 1,
                disarm_reason = 'disarmed_deploy',
                control_hash = NULL,
                control_contract = NULL,
                updated_at = now()
             RETURNING control_epoch",
        )
        .bind(fingerprint)
        .bind(policy_hash)
        .bind(MAXIMUM_REVIEWED_INPUT_WEI.to_string())
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| "route disarmed deployment failed")?;
        if fingerprint == route_fingerprint {
            route_epoch = Some(epoch);
        }
    }
    let route_epoch = route_epoch.ok_or("default route control was not installed")?;
    sqlx::query(
        "UPDATE live_canary.revenue_lane_controls
         SET armed = false, kill_switch = true,
             disarm_reason = 'disarmed_deploy',
             control_epoch = control_epoch + 1, updated_at = now()
         WHERE lane IN ('atlas_solver', 'aave_liquidation')",
    )
    .execute(&mut *transaction)
    .await
    .map_err(|_| "revenue lane disarmed deployment failed")?;
    sqlx::query(
        "UPDATE live_canary.autonomous_candidates
         SET status = 'disarmed', updated_at = now()
         WHERE status IN (
             'materialized', 'approval_pending', 'approved',
             'request_materialized', 'claimed', 'signed'
         )",
    )
    .execute(&mut *transaction)
    .await
    .map_err(|_| "candidate disarm failed")?;
    let next_epoch = previous.control_epoch + 1;
    sqlx::query(
        "UPDATE live_canary.economic_control
         SET phase = 'DISARMED_DEPLOY', route_fingerprint = $1,
             current_size_level = 'MIN', current_input_wei = $2::numeric,
             maximum_reviewed_input_wei = $3::numeric, release_sha = $4,
             engine_image_digest = $5, route_universe_hash = $6,
             route_policy_hash = $7, risk_policy_hash = $7,
             executor_code_hash = $8, readiness_id = NULL,
             authorization_id = NULL, cooldown_until = NULL,
             gas_reserve_wei = 0, gas_reserve_floor_wei = 0,
             control_epoch = $9, last_transition_reason = 'disarmed_deploy',
             state_hash = NULL, updated_at = now()
         WHERE singleton",
    )
    .bind(route_fingerprint)
    .bind(SizeLevel::Min.amount_wei().to_string())
    .bind(MAXIMUM_REVIEWED_INPUT_WEI.to_string())
    .bind(&release_sha)
    .bind(&engine_image_digest)
    .bind(route_universe_hash)
    .bind(route_policy_hash)
    .bind(&executor_code_hash)
    .bind(next_epoch)
    .execute(&mut *transaction)
    .await
    .map_err(|_| "economic disarmed deployment failed")?;
    insert_transition(
        &mut transaction,
        &previous,
        EconomicPhase::DisarmedDeploy,
        SizeLevel::Min,
        "disarmed_deploy",
        None,
        Some(&release_sha),
        next_epoch,
    )
    .await?;
    let active_attempts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM live_canary.execution_attempts
         WHERE status IN (
            'claimed', 'nonce_allocated', 'submission_unknown', 'pending', 'timed_out'
         )",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "active-attempt inspection failed")?;
    if active_attempts != 0 {
        return Err("disarmed deployment is blocked by an active attempt".into());
    }
    let active_revenue_lane: Option<String> = sqlx::query_scalar(
        "SELECT active_lane
         FROM live_canary.global_revenue_submission_lock
         WHERE singleton
         FOR UPDATE",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "global revenue submission lock is unavailable")?;
    if active_revenue_lane.is_some() {
        return Err("disarmed deployment is blocked by an active revenue submission".into());
    }
    let active_atlas_requests: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM live_canary.atlas_solver_requests
         WHERE status IN ('claimed', 'signed', 'submitted', 'submission_unknown')",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "active Atlas request inspection failed")?;
    if active_atlas_requests != 0 {
        return Err("disarmed deployment is blocked by an active Atlas request".into());
    }
    transaction
        .commit()
        .await
        .map_err(|_| "disarmed deployment commit failed")?;
    println!(
        "DISARMED_DEPLOY_OK: release={} route={} global_epoch={} route_epoch={} armed=false kill_switch=true",
        release_sha, route_fingerprint, global_epoch, route_epoch
    );
    Ok(())
}

async fn start_evidence() -> ControlResult<()> {
    if required("PHOENIX_EVIDENCE_START_ACK")? != EVIDENCE_START_ACK {
        return Err("evidence-start acknowledgement is invalid".into());
    }
    require_signerless_control()?;
    evidence_start(&database_pool().await?).await
}

async fn evidence_start(pool: &PgPool) -> ControlResult<()> {
    require_schema(pool).await?;
    let release_sha = required("PHOENIX_RELEASE_SHA")?;
    if !canonical_hex(&release_sha, 40) {
        return Err("release SHA is invalid".into());
    }
    let engine_image_digest = image_digest(&required("PHOENIX_ENGINE_IMAGE")?)?;
    let policy: Value = serde_json::from_str(POLICY).map_err(|_| "route policy is invalid")?;
    let universe: Value =
        serde_json::from_str(UNIVERSE).map_err(|_| "route universe is invalid")?;
    verify_hash(
        &policy,
        "policy_hash",
        "route-policy",
        "phoenix.route-policy.v1",
    )?;
    verify_hash(
        &universe,
        "universe_hash",
        "route-universe",
        "phoenix.route-universe.v1",
    )?;
    let route_fingerprint = value_text(&policy, "route_fingerprint")?;
    let route_policy_hash = value_text(&policy, "policy_hash")?;
    let route_universe_hash = value_text(&universe, "universe_hash")?;

    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| "database transaction failed")?;
    let previous = economic_state_for_update(&mut transaction).await?;
    let controls = sqlx::query(
        "SELECT c.armed AS legacy_armed, c.kill_switch AS legacy_kill_switch,
                g.armed AS global_armed, g.kill_switch AS global_kill_switch,
                g.execution_mode = 'disarmed' AS global_disarmed,
                r.enabled AS route_enabled, r.kill_switch AS route_kill_switch
         FROM live_canary.control c
         CROSS JOIN live_canary.autonomous_global_control g
         JOIN live_canary.autonomous_route_controls r
           ON r.route_fingerprint = $1
         WHERE c.singleton AND g.singleton
         FOR UPDATE OF c, g, r",
    )
    .bind(route_fingerprint)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "disarmed controls are unavailable")?;
    let active_attempts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM live_canary.execution_attempts
         WHERE status IN (
            'claimed', 'nonce_allocated', 'submission_unknown', 'pending', 'timed_out'
         )",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "active-attempt inspection failed")?;
    let unresolved_receipts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM live_canary.execution_attempts
         WHERE status IN ('submission_unknown', 'pending', 'timed_out')",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "receipt-reconciliation inspection failed")?;
    let authority = EvidenceAuthorityState {
        legacy_armed: controls
            .try_get("legacy_armed")
            .map_err(|_| "disarmed controls are invalid")?,
        legacy_kill_switch: controls
            .try_get("legacy_kill_switch")
            .map_err(|_| "disarmed controls are invalid")?,
        global_armed: controls
            .try_get("global_armed")
            .map_err(|_| "disarmed controls are invalid")?,
        global_kill_switch: controls
            .try_get("global_kill_switch")
            .map_err(|_| "disarmed controls are invalid")?,
        global_disarmed: controls
            .try_get("global_disarmed")
            .map_err(|_| "disarmed controls are invalid")?,
        route_enabled: controls
            .try_get("route_enabled")
            .map_err(|_| "disarmed controls are invalid")?,
        route_kill_switch: controls
            .try_get("route_kill_switch")
            .map_err(|_| "disarmed controls are invalid")?,
        active_attempts,
        unresolved_receipts,
    };
    validate_evidence_start(
        &previous,
        &release_sha,
        &engine_image_digest,
        route_fingerprint,
        route_universe_hash,
        route_policy_hash,
        &authority,
    )?;

    let transitioned_at: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| "database clock is unavailable")?;
    let next_epoch = previous.control_epoch + 1;
    let updated = sqlx::query(
        "UPDATE live_canary.economic_control
         SET phase = 'DISARMED_EVIDENCE', control_epoch = $1,
             last_transition_reason = 'disarmed_evidence_started',
             updated_at = $2
         WHERE singleton AND phase = 'DISARMED_DEPLOY'",
    )
    .bind(next_epoch)
    .bind(transitioned_at)
    .execute(&mut *transaction)
    .await
    .map_err(|_| "disarmed evidence transition failed")?;
    if updated.rows_affected() != 1 {
        return Err("economic control is not in DISARMED_DEPLOY".into());
    }
    insert_transition_at(
        &mut transaction,
        &previous,
        EconomicPhase::DisarmedEvidence,
        SizeLevel::Min,
        "disarmed_evidence_started",
        None,
        Some(&release_sha),
        next_epoch,
        transitioned_at,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|_| "disarmed evidence commit failed")?;
    println!(
        "DISARMED_EVIDENCE_OK: release={} route={} control_epoch={} armed=false kill_switch=true transitioned_at={}",
        release_sha,
        route_fingerprint,
        next_epoch,
        transitioned_at.to_rfc3339_opts(SecondsFormat::Micros, true)
    );
    Ok(())
}

async fn create_readiness(pool: &PgPool) -> ControlResult<()> {
    if required("PHOENIX_CANARY_READINESS_ACK")? != READINESS_ACK {
        return Err("readiness acknowledgement is invalid".into());
    }
    require_schema(pool).await?;
    let (contract, source) =
        read_control_contract("PHOENIX_CANARY_READINESS_FILE", "readiness file is invalid")?;
    verify_hash(
        &contract,
        "readiness_hash",
        "canary-readiness",
        "phoenix.canary-readiness.v1",
    )?;
    let input: ReadinessFile =
        serde_json::from_value(contract.clone()).map_err(|_| "readiness file is invalid")?;
    if input.readiness_hash != value_text(&contract, "readiness_hash")? {
        return Err("readiness hash is invalid".into());
    }
    let now = Utc::now();
    input
        .evidence
        .validate()
        .map_err(|_| "readiness evidence gate failed")?;
    input
        .binding
        .validate(now)
        .map_err(|_| "readiness binding failed")?;

    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| "database transaction failed")?;
    let previous = economic_state_for_update(&mut transaction).await?;
    validate_readiness_against_evidence(&previous, &input.binding)?;
    let controls = sqlx::query(
        "SELECT NOT c.armed AND c.kill_switch
                    AND NOT g.armed AND g.kill_switch AND g.execution_mode = 'disarmed'
                    AND NOT r.enabled AND r.kill_switch
                    AND r.route_policy_hash = $2 AS closed,
                g.control_epoch AS global_control_epoch,
                r.control_epoch AS route_control_epoch
         FROM live_canary.control c
         CROSS JOIN live_canary.autonomous_global_control g
         JOIN live_canary.autonomous_route_controls r
           ON r.route_fingerprint = $1
         WHERE c.singleton AND g.singleton
         FOR UPDATE OF c, g, r",
    )
    .bind(&input.binding.route_fingerprint)
    .bind(&input.binding.route_policy_hash)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "disarmed controls are unavailable")?;
    if !controls
        .try_get::<bool, _>("closed")
        .map_err(|_| "disarmed controls are invalid")?
    {
        return Err("readiness cannot be created while execution authority is open".into());
    }
    let global_epoch: i64 = controls
        .try_get("global_control_epoch")
        .map_err(|_| "disarmed controls are invalid")?;
    let route_epoch: i64 = controls
        .try_get("route_control_epoch")
        .map_err(|_| "disarmed controls are invalid")?;
    if u64::try_from(global_epoch).ok() != Some(input.binding.global_control_epoch)
        || u64::try_from(route_epoch).ok() != Some(input.binding.route_control_epoch)
    {
        return Err("readiness does not bind the current fail-closed control epochs".into());
    }
    let active_attempts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM live_canary.execution_attempts
         WHERE status IN (
            'claimed', 'nonce_allocated', 'submission_unknown', 'pending', 'timed_out'
         )",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "active-attempt inspection failed")?;
    if active_attempts != 0 {
        return Err("readiness is blocked by an active execution attempt".into());
    }
    sqlx::query(
        "INSERT INTO live_canary.canary_readiness_records(
            readiness_id, schema_version, release_sha, engine_image_digest,
            route_fingerprint, route_universe_hash, route_policy_hash,
            risk_policy_hash, economic_control_epoch,
            global_control_epoch, route_control_epoch,
            executor_code_hash, contract_identity_hash, wallet_gas_reserve_wei,
            gas_reserve_floor_wei, current_daily_loss_wei, daily_loss_limit_wei,
            observed_from, observed_until, candidate_evidence_hashes,
            evidence_metrics, readiness_contract, readiness_hash, created_at, expires_at
         ) VALUES (
            $1, 'phoenix.canary-readiness.v1', $2, $3, $4, $5, $6, $7,
            $8, $9, $10, $11, $12, $13::numeric, $14::numeric, $15::numeric,
            $16::numeric, $17, $18, $19, $20, $21, $22, $23, $24
         )",
    )
    .bind(input.readiness_id)
    .bind(&input.binding.release_sha)
    .bind(&input.binding.engine_image_digest)
    .bind(&input.binding.route_fingerprint)
    .bind(&input.binding.route_universe_hash)
    .bind(&input.binding.route_policy_hash)
    .bind(&input.binding.risk_policy_hash)
    .bind(
        i64::try_from(input.binding.economic_control_epoch)
            .map_err(|_| "control epoch is invalid")?,
    )
    .bind(
        i64::try_from(input.binding.global_control_epoch)
            .map_err(|_| "control epoch is invalid")?,
    )
    .bind(i64::try_from(input.binding.route_control_epoch).map_err(|_| "control epoch is invalid")?)
    .bind(&input.binding.executor_code_hash)
    .bind(&input.binding.contract_identity_hash)
    .bind(input.binding.wallet_gas_reserve_wei.to_string())
    .bind(input.binding.gas_reserve_floor_wei.to_string())
    .bind(input.binding.current_daily_loss_wei.to_string())
    .bind(input.binding.daily_loss_limit_wei.to_string())
    .bind(input.binding.observed_from)
    .bind(input.binding.observed_until)
    .bind(sqlx::types::Json(&input.binding.candidate_evidence_hashes))
    .bind(sqlx::types::Json(&input.evidence))
    .bind(sqlx::types::Json(&contract))
    .bind(&input.readiness_hash)
    .bind(input.binding.created_at)
    .bind(input.binding.expires_at)
    .execute(&mut *transaction)
    .await
    .map_err(|_| "readiness persistence failed")?;
    let next_epoch = previous.control_epoch + 1;
    sqlx::query(
        "UPDATE live_canary.economic_control
         SET phase = 'CANARY_READY', readiness_id = $1,
             route_fingerprint = $2, route_policy_hash = $3,
             risk_policy_hash = $3,
             gas_reserve_wei = $4::numeric, gas_reserve_floor_wei = $5::numeric,
             control_epoch = $6, last_transition_reason = 'evidence_gate_passed',
             updated_at = now()
         WHERE singleton",
    )
    .bind(input.readiness_id)
    .bind(&input.binding.route_fingerprint)
    .bind(&input.binding.route_policy_hash)
    .bind(input.binding.wallet_gas_reserve_wei.to_string())
    .bind(input.binding.gas_reserve_floor_wei.to_string())
    .bind(next_epoch)
    .execute(&mut *transaction)
    .await
    .map_err(|_| "readiness transition failed")?;
    insert_transition(
        &mut transaction,
        &previous,
        EconomicPhase::CanaryReady,
        SizeLevel::Min,
        "evidence_gate_passed",
        Some(&input.readiness_hash),
        Some(&input.binding.release_sha),
        next_epoch,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|_| "readiness commit failed")?;
    println!(
        "CANARY_READY_OK: readiness_id={} expires_at={} source={}",
        input.readiness_id,
        input
            .binding
            .expires_at
            .to_rfc3339_opts(SecondsFormat::Secs, true),
        source.display()
    );
    Ok(())
}

async fn install_authorization(pool: &PgPool) -> ControlResult<()> {
    if required("PHOENIX_AUTOMATION_AUTHORIZATION_ACK")? != AUTHORIZATION_ACK {
        return Err("automation authorization acknowledgement is invalid".into());
    }
    require_schema(pool).await?;
    let (contract, source) = read_control_contract(
        "PHOENIX_AUTOMATION_AUTHORIZATION_FILE",
        "automation authorization file is invalid",
    )?;
    verify_hash(
        &contract,
        "authorization_hash",
        "automation-authorization",
        "phoenix.automation-authorization.v1",
    )?;
    let input: AuthorizationFile = serde_json::from_value(contract.clone())
        .map_err(|_| "automation authorization file is invalid")?;
    if input.authorization_hash != value_text(&contract, "authorization_hash")? {
        return Err("automation authorization hash is invalid".into());
    }
    let current: (String, String, String) = sqlx::query_as(
        "SELECT phase, route_fingerprint, route_policy_hash
         FROM live_canary.economic_control WHERE singleton",
    )
    .fetch_one(pool)
    .await
    .map_err(|_| "economic control is unavailable")?;
    if current.0 != EconomicPhase::CanaryReady.as_str()
        || current.1 != input.authorization.route_fingerprint
        || current.2 != input.authorization.route_policy_hash
    {
        return Err("authorization does not bind the current ready canary".into());
    }
    if input.authorization.maximum_reviewed_input_wei != MAXIMUM_REVIEWED_INPUT_WEI
        || !input.authorization.one_transaction_at_a_time
        || !input.authorization.reviewed_ladder_only
        || !input.authorization.automatic_disarm_required
        || Utc::now() >= input.authorization.expires_at
    {
        return Err("automation authorization is outside the reviewed bounds".into());
    }
    sqlx::query(
        "INSERT INTO live_canary.automation_authorizations(
            authorization_id, schema_version, route_fingerprint,
            route_policy_hash, maximum_reviewed_input_wei,
            executor_code_hash, release_family, one_transaction_at_a_time,
            reviewed_ladder_only, automatic_disarm_required,
            authorization_contract, authorization_hash, authorized_at, expires_at
         ) VALUES (
            $1, 'phoenix.automation-authorization.v1', $2, $3, $4::numeric,
            $5, $6, $7, $8, $9, $10, $11, now(), $12
         )",
    )
    .bind(input.authorization_id)
    .bind(&input.authorization.route_fingerprint)
    .bind(&input.authorization.route_policy_hash)
    .bind(input.authorization.maximum_reviewed_input_wei.to_string())
    .bind(&input.authorization.executor_code_hash)
    .bind(&input.authorization.release_family)
    .bind(input.authorization.one_transaction_at_a_time)
    .bind(input.authorization.reviewed_ladder_only)
    .bind(input.authorization.automatic_disarm_required)
    .bind(sqlx::types::Json(&contract))
    .bind(&input.authorization_hash)
    .bind(input.authorization.expires_at)
    .execute(pool)
    .await
    .map_err(|_| "automation authorization persistence failed")?;
    println!(
        "AUTOMATION_AUTHORIZATION_OK: authorization_id={} source={}",
        input.authorization_id,
        source.display()
    );
    Ok(())
}

async fn activate(pool: &PgPool) -> ControlResult<()> {
    if required("PHOENIX_AUTONOMOUS_ACTIVATION_ACK")? != ACTIVATE_ACK {
        return Err("activation acknowledgement is invalid".into());
    }
    require_schema(pool).await?;
    let readiness_id = Uuid::parse_str(&required("PHOENIX_CANARY_READINESS_ID")?)
        .map_err(|_| "readiness ID is invalid")?;
    let authorization_id = Uuid::parse_str(&required("PHOENIX_AUTOMATION_AUTHORIZATION_ID")?)
        .map_err(|_| "authorization ID is invalid")?;
    let configured_maximum = required_u128("LIVE_EXECUTOR_MAX_INPUT_AMOUNT")?;
    let maximum_input = SizeLevel::Min.amount_wei();
    let daily_loss_limit = required_u128("LIVE_EXECUTOR_MAX_DAILY_LOSS_WEI")?;
    if daily_loss_limit == 0 {
        return Err("global daily loss limit is economically inert".into());
    }

    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| "database transaction failed")?;
    let previous = economic_state_for_update(&mut transaction).await?;
    let selected_route = previous
        .route_fingerprint
        .as_deref()
        .ok_or("economic route is unavailable")?;
    let policy = reviewed_policy_value(selected_route)?;
    if policy
        .get("enabled_for_autonomous_live")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err("route policy is not enabled for autonomous LIVE".into());
    }
    let policy_minimum = value_u128(&policy, "minimum_input_amount")?;
    let policy_maximum = value_u128(&policy, "maximum_input_amount")?;
    if configured_maximum != MAXIMUM_REVIEWED_INPUT_WEI
        || policy_maximum != MAXIMUM_REVIEWED_INPUT_WEI
        || policy_minimum != SizeLevel::Min.amount_wei()
    {
        return Err("configured maximum does not match the reviewed ladder".into());
    }
    if previous.phase != EconomicPhase::CanaryReady
        || previous.readiness_id != Some(readiness_id)
        || policy.get("route_fingerprint").and_then(Value::as_str) != Some(selected_route)
    {
        return Err("economic control is not ready for this canary".into());
    }
    let record = sqlx::query(
        "SELECT readiness_record.readiness_contract,
                automation_authorization.authorization_contract
         FROM live_canary.canary_readiness_records readiness_record
         JOIN live_canary.automation_authorizations automation_authorization
           ON automation_authorization.authorization_id = $2
         WHERE readiness_record.readiness_id = $1
           AND automation_authorization.consumed_at IS NULL
         FOR UPDATE OF readiness_record, automation_authorization",
    )
    .bind(readiness_id)
    .bind(authorization_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "readiness or authorization is unavailable")?;
    let readiness_contract: sqlx::types::Json<Value> = record
        .try_get("readiness_contract")
        .map_err(|_| "readiness contract is invalid")?;
    let authorization_contract: sqlx::types::Json<Value> = record
        .try_get("authorization_contract")
        .map_err(|_| "authorization contract is invalid")?;
    verify_hash(
        &readiness_contract.0,
        "readiness_hash",
        "canary-readiness",
        "phoenix.canary-readiness.v1",
    )?;
    verify_hash(
        &authorization_contract.0,
        "authorization_hash",
        "automation-authorization",
        "phoenix.automation-authorization.v1",
    )?;
    let readiness: ReadinessFile = serde_json::from_value(readiness_contract.0)
        .map_err(|_| "readiness contract is invalid")?;
    let authorization: AuthorizationFile = serde_json::from_value(authorization_contract.0)
        .map_err(|_| "authorization contract is invalid")?;
    if readiness.readiness_id != readiness_id
        || authorization.authorization_id != authorization_id
        || readiness.binding.release_sha != previous.release_sha.as_deref().unwrap_or_default()
        || readiness.binding.engine_image_digest
            != previous.engine_image_digest.as_deref().unwrap_or_default()
        || readiness.binding.route_universe_hash
            != previous.route_universe_hash.as_deref().unwrap_or_default()
        || readiness.binding.route_policy_hash
            != previous.route_policy_hash.as_deref().unwrap_or_default()
        || readiness.binding.risk_policy_hash
            != previous.risk_policy_hash.as_deref().unwrap_or_default()
        || readiness.binding.executor_code_hash
            != previous.executor_code_hash.as_deref().unwrap_or_default()
        || u64::try_from(previous.control_epoch - 1).ok()
            != Some(readiness.binding.economic_control_epoch)
    {
        return Err("ready canary binding does not match economic control".into());
    }
    let now = Utc::now();
    let decision = activate_canary(
        previous.phase,
        &readiness.binding,
        &readiness.evidence,
        &authorization.authorization,
        now,
    )
    .map_err(|_| "ready canary activation gate failed")?;
    if decision
        != (Transition::Promote {
            phase: EconomicPhase::LiveCanaryMin,
            level: SizeLevel::Min,
        })
    {
        return Err("first canary decision is not the minimum level".into());
    }
    let active_count: i64 = sqlx::query_scalar(
        "SELECT count(*)
         FROM live_canary.execution_attempts
         WHERE status IN (
             'claimed', 'nonce_allocated', 'submission_unknown', 'pending', 'timed_out'
         )",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "active-attempt inspection failed")?;
    if active_count != 0 {
        return Err("activation is blocked by an active execution attempt".into());
    }
    let global_epoch: i64 = sqlx::query_scalar(
        "SELECT control_epoch + 1
         FROM live_canary.autonomous_global_control
         WHERE singleton
         FOR UPDATE",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "global control is unavailable")?;
    let route_fingerprint = value_text(&policy, "route_fingerprint")?;
    let route_epoch = sqlx::query_scalar::<_, i64>(
        "SELECT control_epoch + 1
         FROM live_canary.autonomous_route_controls
         WHERE route_fingerprint = $1
         FOR UPDATE",
    )
    .bind(route_fingerprint)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|_| "route control inspection failed")?
    .unwrap_or(0);
    if u64::try_from(global_epoch - 1).ok() != Some(readiness.binding.global_control_epoch)
        || u64::try_from(route_epoch - 1).ok() != Some(readiness.binding.route_control_epoch)
    {
        return Err("control epoch changed after readiness was created".into());
    }
    let updated_at = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    let mut global = json!({
        "schema_version": "phoenix.autonomous-global-control.v1",
        "chain_id": 42161,
        "armed": true,
        "kill_switch": false,
        "execution_mode": "live",
        "maximum_input_amount": maximum_input.to_string(),
        "daily_loss_limit": daily_loss_limit.to_string(),
        "daily_ordering_budget": "0",
        "maximum_concurrent_candidates": 1,
        "control_epoch": global_epoch,
        "updated_at": updated_at,
        "disarm_reason": Value::Null,
        "control_hash": "0".repeat(64)
    });
    set_hash(
        &mut global,
        "control_hash",
        "global-control",
        "phoenix.autonomous-global-control.v1",
    )?;
    let mut route = json!({
        "schema_version": "phoenix.autonomous-route-control.v1",
        "chain_id": 42161,
        "route_fingerprint": route_fingerprint,
        "route_policy_hash": value_text(&policy, "policy_hash")?,
        "enabled": true,
        "kill_switch": false,
        "current_size_level": "MIN",
        "maximum_permitted_size": maximum_input.to_string(),
        "daily_loss_limit": value_text(&policy, "per_route_daily_loss")?,
        "maximum_consecutive_losses": policy.get("maximum_consecutive_losses")
            .and_then(Value::as_u64)
            .ok_or("route loss policy is invalid")?,
        "submission_unknown_disarms": true,
        "integrity_failure_disarms": true,
        "cooldown_until": Value::Null,
        "control_epoch": route_epoch,
        "updated_at": updated_at,
        "disarm_reason": Value::Null,
        "control_hash": "0".repeat(64)
    });
    set_hash(
        &mut route,
        "control_hash",
        "route-control",
        "phoenix.autonomous-route-control.v1",
    )?;
    sqlx::query(
        "UPDATE live_canary.control
         SET armed = true, kill_switch = false, disarm_reason = 'armed',
             updated_at = $1::timestamptz
         WHERE singleton",
    )
    .bind(&updated_at)
    .execute(&mut *transaction)
    .await
    .map_err(|_| "legacy execution control activation failed")?;
    sqlx::query(
        "UPDATE live_canary.autonomous_global_control
         SET armed = true, kill_switch = false, execution_mode = 'live',
             maximum_input_amount = $1::numeric, daily_loss_limit = $2::numeric,
             daily_ordering_budget = 0, maximum_concurrent_candidates = 1,
             control_epoch = $3, disarm_reason = NULL, control_hash = $4,
             control_contract = $5, updated_at = $6::timestamptz
         WHERE singleton",
    )
    .bind(maximum_input.to_string())
    .bind(daily_loss_limit.to_string())
    .bind(global_epoch)
    .bind(value_text(&global, "control_hash")?)
    .bind(sqlx::types::Json(&global))
    .bind(&updated_at)
    .execute(&mut *transaction)
    .await
    .map_err(|_| "global autonomous control activation failed")?;
    sqlx::query(
        "INSERT INTO live_canary.autonomous_route_controls(
            route_fingerprint, route_policy_hash, enabled, kill_switch,
            current_size_level, maximum_permitted_size, cooldown_until,
            control_epoch, disarm_reason, control_hash, control_contract, updated_at
         ) VALUES (
            $1, $2, true, false, 'MIN', $3::numeric, NULL,
            $4, NULL, $5, $6, $7::timestamptz
         )
         ON CONFLICT (route_fingerprint) DO UPDATE SET
            route_policy_hash = EXCLUDED.route_policy_hash,
            enabled = EXCLUDED.enabled,
            kill_switch = EXCLUDED.kill_switch,
            current_size_level = EXCLUDED.current_size_level,
            maximum_permitted_size = EXCLUDED.maximum_permitted_size,
            cooldown_until = EXCLUDED.cooldown_until,
            control_epoch = EXCLUDED.control_epoch,
            disarm_reason = EXCLUDED.disarm_reason,
            control_hash = EXCLUDED.control_hash,
            control_contract = EXCLUDED.control_contract,
            updated_at = EXCLUDED.updated_at",
    )
    .bind(route_fingerprint)
    .bind(value_text(&policy, "policy_hash")?)
    .bind(maximum_input.to_string())
    .bind(route_epoch)
    .bind(value_text(&route, "control_hash")?)
    .bind(sqlx::types::Json(&route))
    .bind(&updated_at)
    .execute(&mut *transaction)
    .await
    .map_err(|_| "route autonomous control activation failed")?;
    let economic_epoch = previous.control_epoch + 1;
    sqlx::query(
        "UPDATE live_canary.economic_control
         SET phase = 'LIVE_CANARY_MIN', current_size_level = 'MIN',
             current_input_wei = $1::numeric, authorization_id = $2,
             cooldown_until = NULL, control_epoch = $3,
             last_transition_reason = 'owner_authorized_min_canary',
             updated_at = $4::timestamptz
         WHERE singleton AND phase = 'CANARY_READY' AND readiness_id = $5",
    )
    .bind(maximum_input.to_string())
    .bind(authorization_id)
    .bind(economic_epoch)
    .bind(&updated_at)
    .bind(readiness_id)
    .execute(&mut *transaction)
    .await
    .map_err(|_| "economic canary activation failed")?;
    let authorization_consumed = sqlx::query(
        "UPDATE live_canary.automation_authorizations
         SET consumed_at = now()
         WHERE authorization_id = $1
           AND consumed_at IS NULL
           AND expires_at > now()",
    )
    .bind(authorization_id)
    .execute(&mut *transaction)
    .await
    .map_err(|_| "automation authorization consumption failed")?;
    if authorization_consumed.rows_affected() != 1 {
        return Err("automation authorization is already consumed or expired".into());
    }
    insert_transition(
        &mut transaction,
        &previous,
        EconomicPhase::LiveCanaryMin,
        SizeLevel::Min,
        "owner_authorized_min_canary",
        Some(&readiness.readiness_hash),
        previous.release_sha.as_deref(),
        economic_epoch,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|_| "activation commit failed")?;
    println!(
        "AUTONOMOUS_ACTIVATION_OK: chain=42161 route={} level=MIN input_wei={} global_epoch={} route_epoch={}",
        route_fingerprint, maximum_input, global_epoch, route_epoch
    );
    Ok(())
}

async fn materialize_activation_contracts() -> ControlResult<()> {
    if required("PHOENIX_ACTIVATION_MATERIALIZATION_ACK")? != MATERIALIZE_ACTIVATION_ACK {
        return Err("activation materialization acknowledgement is invalid".into());
    }
    require_signerless_control()?;
    let (value, _) = read_control_contract(
        "PHOENIX_ACTIVATION_REQUEST_FILE",
        "activation request file is invalid",
    )?;
    verify_hash(
        &value,
        "request_hash",
        "economic-activation-request",
        ACTIVATION_REQUEST_SCHEMA,
    )?;
    let request: ActivationRequest =
        serde_json::from_value(value).map_err(|_| "activation request file is invalid")?;
    let pool = database_pool().await?;
    let database_now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&pool)
        .await
        .map_err(|_| "database clock is unavailable")?;
    request
        .validate(database_now)
        .map_err(|_| "activation request validation failed")?;
    let current = collect_activation_request(&pool, Some(&request))
        .await?
        .ok_or("activation request no longer has eligible Production evidence")?;
    if current.candidate != request.candidate
        || current.binding.release_sha != request.binding.release_sha
        || current.binding.engine_image_digest != request.binding.engine_image_digest
        || current.binding.route_universe_hash != request.binding.route_universe_hash
        || current.binding.route_policy_hash != request.binding.route_policy_hash
        || current.binding.risk_policy_hash != request.binding.risk_policy_hash
        || current.binding.economic_control_epoch != request.binding.economic_control_epoch
        || current.binding.global_control_epoch != request.binding.global_control_epoch
        || current.binding.route_control_epoch != request.binding.route_control_epoch
        || current.binding.executor_code_hash != request.binding.executor_code_hash
        || current.binding.contract_identity_hash != request.binding.contract_identity_hash
    {
        return Err("activation request no longer binds current Production state".into());
    }

    let created_at = current.created_at;
    let mut binding = current.binding;
    binding.observed_until = created_at - ChronoDuration::microseconds(1);
    binding.created_at = created_at;
    binding.expires_at = created_at
        + ChronoDuration::seconds(phoenix_live_executor::economic_control::READINESS_TTL_SECONDS);
    binding
        .validate(created_at)
        .map_err(|_| "materialized readiness binding is invalid")?;
    current
        .evidence
        .validate()
        .map_err(|_| "materialized readiness evidence is invalid")?;
    let mut readiness = json!({
        "schema_version": "phoenix.canary-readiness.v1",
        "readiness_id": Uuid::new_v4(),
        "binding": binding,
        "evidence": current.evidence,
        "readiness_hash": "0".repeat(64)
    });
    set_hash(
        &mut readiness,
        "readiness_hash",
        "canary-readiness",
        "phoenix.canary-readiness.v1",
    )?;

    let authorization = AutomationAuthorization {
        route_fingerprint: current.candidate.route_fingerprint,
        route_policy_hash: current.candidate.route_policy_hash,
        maximum_reviewed_input_wei: MAXIMUM_REVIEWED_INPUT_WEI,
        executor_code_hash: current.candidate.executor_code_hash,
        release_family: "phoenix-v4".to_string(),
        one_transaction_at_a_time: true,
        reviewed_ladder_only: true,
        automatic_disarm_required: true,
        expires_at: binding.expires_at,
    };
    authorization
        .validate(&binding, created_at)
        .map_err(|_| "materialized automation authorization is invalid")?;
    let mut authorization_contract = json!({
        "schema_version": "phoenix.automation-authorization.v1",
        "authorization_id": Uuid::new_v4(),
        "authorization": authorization,
        "authorization_hash": "0".repeat(64)
    });
    set_hash(
        &mut authorization_contract,
        "authorization_hash",
        "automation-authorization",
        "phoenix.automation-authorization.v1",
    )?;
    let payload = ActivationMaterialization {
        schema_version: "phoenix.activation-materialization.v1",
        request_id: request.request_id,
        request_hash: request.request_hash,
        readiness,
        authorization: authorization_contract,
    };
    println!(
        "{}",
        serde_json::to_string(&payload)
            .map_err(|_| "activation materialization serialization failed")?
    );
    Ok(())
}

async fn supervise_economic_control() -> ControlResult<()> {
    let live_interval = env::var("PHOENIX_ECONOMIC_SUPERVISOR_INTERVAL_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (30..=60).contains(value))
        .unwrap_or(45);
    let evidence_interval = env::var("PHOENIX_ACTIVATION_REQUEST_INTERVAL_MILLISECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (250..=2_000).contains(value))
        .unwrap_or(500);
    let pool = database_pool().await?;
    let mut emitted_fork_result = None;
    let mut last_revenue_provider_check = None;
    loop {
        if last_revenue_provider_check
            .map(|checked: Instant| checked.elapsed() >= REVENUE_PROVIDER_AUTHORITY_CHECK_INTERVAL)
            .unwrap_or(true)
        {
            if let Err(error) = converge_revenue_provider_authority(&pool).await {
                eprintln!("REVENUE_PROVIDER_AUTHORITY_CHECK_FAILED: {error}");
            }
            last_revenue_provider_check = Some(Instant::now());
        }
        let delay = if current_economic_phase(&pool).await? == EconomicPhase::DisarmedEvidence {
            match collect_activation_request(&pool, None).await {
                Ok(Some(request))
                    if emitted_fork_result.as_deref()
                        != Some(request.candidate.fork_result_hash.as_str()) =>
                {
                    let outbox_value = required(ACTIVATION_REQUEST_OUTBOX_ENV)?;
                    let outbox = Path::new(&outbox_value);
                    match write_atomic_request(outbox, &request) {
                        Ok(path) => {
                            emitted_fork_result = Some(request.candidate.fork_result_hash.clone());
                            println!(
                                "ECONOMIC_ACTIVATION_REQUEST_READY: request_id={} candidate_id={} source={}",
                                request.request_id,
                                request.candidate.candidate_id,
                                path.display()
                            );
                        }
                        Err(error) => {
                            eprintln!("ECONOMIC_ACTIVATION_REQUEST_FAILED: {error}");
                        }
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => {}
                Err(error) => eprintln!("ECONOMIC_ACTIVATION_ASSESSMENT_FAILED: {error}"),
            }
            Duration::from_millis(evidence_interval)
        } else {
            if let Err(error) = evaluate_economic_control(&pool).await {
                eprintln!("ECONOMIC_SUPERVISION_STEP_FAILED: {error}");
            }
            Duration::from_secs(live_interval)
        };
        tokio::time::sleep(delay).await;
    }
}

async fn current_economic_phase(pool: &PgPool) -> ControlResult<EconomicPhase> {
    let phase: String =
        sqlx::query_scalar("SELECT phase FROM live_canary.economic_control WHERE singleton")
            .fetch_one(pool)
            .await
            .map_err(|_| "economic control is unavailable")?;
    parse_phase(&phase)
}

fn persistent_hunter_provider_failure(payload: &Value, now_millis: i64) -> ControlResult<bool> {
    let reason = payload
        .get("degraded_reason")
        .and_then(Value::as_str)
        .ok_or("Aave/Atlas degraded reason is invalid")?;
    if !matches!(
        reason,
        "provider_disagreement"
            | "provider_unavailable"
            | "provider_timeout"
            | "provider_rate_limited"
    ) {
        return Ok(false);
    }
    let recovery = payload
        .get("provider_recovery_state")
        .and_then(Value::as_str)
        .ok_or("Aave/Atlas recovery state is invalid")?;
    let class_degradations = payload
        .get("provider_current_class_failure_streak")
        .and_then(Value::as_u64)
        .ok_or("Aave/Atlas current failure streak is invalid")?;
    let circuit_until = payload
        .get("provider_circuit_open_until_unix_millis")
        .and_then(Value::as_i64)
        .ok_or("Aave/Atlas circuit evidence is invalid")?;
    let degraded_since = payload
        .get("provider_degraded_since_unix_millis")
        .and_then(Value::as_u64)
        .and_then(|value| i64::try_from(value).ok())
        .ok_or("Aave/Atlas degradation time is invalid")?;
    Ok(recovery == "recovering"
        && class_degradations >= 2
        && degraded_since > 0
        && now_millis.saturating_sub(degraded_since) >= REVENUE_PROVIDER_FAILURE_MINIMUM_DURATION
        && circuit_until > now_millis)
}

async fn hunter_readiness_payload() -> ControlResult<Value> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| "Aave/Atlas readiness client initialization failed")?;
    let response = client
        .get(ATLAS_HUNTER_READINESS_URL)
        .send()
        .await
        .map_err(|_| "Aave/Atlas hunter readiness is unavailable")?;
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "Aave/Atlas hunter readiness is unavailable")?;
    if bytes.len() > 64 * 1024 {
        return Err("Aave/Atlas hunter readiness is oversized".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Aave/Atlas hunter readiness is invalid".into())
}

async fn fail_close_provider_execution_gate(pool: &PgPool, reason: &str) -> ControlResult<()> {
    let gate = sqlx::query(
        "UPDATE live_canary.revenue_provider_authority
         SET exact_execution_ready=false, gate_reason=$1, gate_updated_at=now(),
             request_evidence_not_before=now(), recovery_status='collecting', sample_count=0,
             sample_1_at=NULL, sample_1_primary_provider=NULL, sample_1_confirmation_provider=NULL,
             sample_2_at=NULL, sample_2_primary_provider=NULL, sample_2_confirmation_provider=NULL,
             sample_3_at=NULL, sample_3_primary_provider=NULL, sample_3_confirmation_provider=NULL,
             updated_at=now()
         WHERE singleton",
    )
    .bind(reason)
    .execute(pool)
    .await
    .map_err(|_| "provider execution gate update failed")?;
    if gate.rows_affected() != 1 {
        return Err("provider execution gate is unavailable".into());
    }
    Ok(())
}

async fn hunter_readiness_or_fail_closed(pool: &PgPool) -> ControlResult<Value> {
    match hunter_readiness_payload().await {
        Ok(payload) => Ok(payload),
        Err(error) => {
            fail_close_provider_execution_gate(pool, "hunter_readiness_unavailable").await?;
            Err(error)
        }
    }
}

async fn converge_revenue_provider_authority(pool: &PgPool) -> ControlResult<()> {
    let rows = sqlx::query(
        "SELECT lane, armed, kill_switch
         FROM live_canary.revenue_lane_controls
         WHERE lane IN ('aave_liquidation', 'atlas_solver')
         ORDER BY lane",
    )
    .fetch_all(pool)
    .await
    .map_err(|_| "revenue lane authority is unavailable")?;
    if rows.len() != 2
        || rows[0].try_get::<String, _>("lane").ok().as_deref() != Some("aave_liquidation")
        || rows[1].try_get::<String, _>("lane").ok().as_deref() != Some("atlas_solver")
    {
        return Err("the exact revenue lane set is unavailable".into());
    }
    let states = rows
        .iter()
        .map(|row| {
            Ok((
                row.try_get::<bool, _>("armed")
                    .map_err(|_| "revenue lane authority is invalid")?,
                row.try_get::<bool, _>("kill_switch")
                    .map_err(|_| "revenue lane authority is invalid")?,
            ))
        })
        .collect::<ControlResult<Vec<_>>>()?;
    let both_closed = states.iter().all(|(armed, kill)| !armed && *kill);
    if both_closed {
        let payload = hunter_readiness_or_fail_closed(pool).await?;
        return attempt_provider_authority_recovery(pool, &payload).await;
    }
    let both_active = states.iter().all(|(armed, kill)| *armed && !kill);
    let reason = if both_active {
        let payload = hunter_readiness_or_fail_closed(pool).await?;
        let degraded_reason = payload
            .get("degraded_reason")
            .and_then(Value::as_str)
            .ok_or("Aave/Atlas degraded reason is invalid")?;
        let exact_ready = payload
            .get("exact_execution_readiness")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !exact_ready {
            let gate_reason = if matches!(
                degraded_reason,
                "provider_disagreement"
                    | "provider_unavailable"
                    | "provider_timeout"
                    | "provider_rate_limited"
                    | "revenue_lane_authority_diverged"
            ) {
                degraded_reason
            } else {
                "exact_state_stale_or_incomplete"
            };
            fail_close_provider_execution_gate(pool, gate_reason).await?;
        }
        if matches!(
            degraded_reason,
            "provider_disagreement"
                | "provider_unavailable"
                | "provider_timeout"
                | "provider_rate_limited"
                | "revenue_lane_authority_diverged"
        ) {}
        if degraded_reason == "revenue_lane_authority_diverged" {
            "revenue_lane_authority_diverged"
        } else if persistent_hunter_provider_failure(&payload, Utc::now().timestamp_millis())? {
            match degraded_reason {
                "provider_disagreement" => "provider_disagreement",
                "provider_unavailable" => "provider_unavailable",
                "provider_timeout" => "provider_timeout",
                "provider_rate_limited" => "provider_rate_limited",
                _ => return Err("Aave/Atlas degraded reason is invalid".into()),
            }
        } else {
            return Ok(());
        }
    } else {
        "revenue_lane_authority_diverged"
    };
    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| "revenue fail-close transaction failed")?;
    let previous = economic_state_for_update(&mut transaction).await?;
    let transitioned_at = Utc::now();
    fail_close_execution_authority(&mut transaction, reason, transitioned_at)
        .await
        .map_err(|_| "revenue provider fail-close failed")?;
    if matches!(
        reason,
        "provider_disagreement"
            | "provider_unavailable"
            | "provider_timeout"
            | "provider_rate_limited"
    ) && previous.phase == EconomicPhase::DisarmedEvidence
        && previous.level == SizeLevel::MaxReviewed
    {
        let release_sha = previous
            .release_sha
            .as_deref()
            .ok_or("provider failure release binding is unavailable")?;
        let recovery = sqlx::query(
            "UPDATE live_canary.revenue_provider_authority
             SET recovery_status = 'collecting', failure_reason = $1,
                 failure_control_epoch = $2, failure_transition_at = $3,
                 failure_release_sha = $4, restore_phase = 'DISARMED_EVIDENCE',
                 restore_size_level = 'MAX_REVIEWED', last_block_reason = NULL,
                 recovery_evidence_hash = NULL, updated_at = now()
             WHERE singleton",
        )
        .bind(reason)
        .bind(previous.control_epoch + 1)
        .bind(transitioned_at)
        .bind(release_sha)
        .execute(&mut *transaction)
        .await
        .map_err(|_| "provider recovery evidence initialization failed")?;
        if recovery.rows_affected() != 1 {
            return Err("provider recovery evidence is unavailable".into());
        }
    }
    transaction
        .commit()
        .await
        .map_err(|_| "revenue provider fail-close commit failed")?;
    println!("REVENUE_PROVIDER_AUTHORITY_DISARMED: reason={reason}");
    Ok(())
}

fn provider_recovery_samples(payload: &Value) -> ControlResult<Vec<(DateTime<Utc>, String)>> {
    if payload.get("hunting_health").and_then(Value::as_bool) != Some(true)
        || payload
            .get("exact_execution_readiness")
            .and_then(Value::as_bool)
            != Some(true)
        || payload.get("atlas_connected").and_then(Value::as_bool) != Some(true)
        || payload
            .get("provider_recovery_state")
            .and_then(Value::as_str)
            != Some("ready")
        || payload
            .get("provider_circuit_open_until_unix_millis")
            .and_then(Value::as_i64)
            .unwrap_or_default()
            > Utc::now().timestamp_millis()
        || payload.get("degraded_reason").and_then(Value::as_str) != Some("")
        || payload.get("primary").and_then(Value::as_str) != Some("production-nownodes-arbitrum")
        || !payload.get("confirmation").is_some_and(Value::is_null)
        || payload.get("quorum").and_then(Value::as_u64) != Some(1)
    {
        return Err("provider recovery readiness is not green".into());
    }
    let values = payload
        .get("provider_recovery_samples")
        .and_then(Value::as_array)
        .ok_or("provider recovery samples are unavailable")?;
    if values.len() != 3 {
        return Err("three provider recovery samples are required".into());
    }
    let mut samples = Vec::with_capacity(3);
    for value in values {
        let observed = value
            .get("observed_at")
            .and_then(Value::as_str)
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc))
            .ok_or("provider recovery timestamp is invalid")?;
        let primary = value
            .get("primary_provider")
            .and_then(Value::as_str)
            .filter(|value| *value == "production-nownodes-arbitrum")
            .ok_or("provider recovery primary identity is invalid")?;
        if !value.get("confirmation").is_some_and(Value::is_null)
            || value.get("quorum").and_then(Value::as_u64) != Some(1)
        {
            return Err("provider recovery single-primary evidence is invalid".into());
        }
        samples.push((observed, primary.to_string()));
    }
    if !(samples[0].0 < samples[1].0 && samples[1].0 < samples[2].0)
        || Utc::now().signed_duration_since(samples[2].0) > ChronoDuration::minutes(10)
    {
        return Err("provider recovery samples are stale or unordered".into());
    }
    Ok(samples)
}

#[derive(Clone, Debug)]
struct ProviderRecoveryTransitionBinding {
    reason: String,
    failure_epoch: i64,
    failure_at: DateTime<Utc>,
    failure_release: String,
    restore_phase: String,
    restore_size: String,
    durable_sample_count: i16,
    durable_samples: Vec<(DateTime<Utc>, String)>,
}

fn validate_provider_recovery_transition_binding(
    previous: &EconomicState,
    release_sha: &str,
    binding: &ProviderRecoveryTransitionBinding,
    samples: &[(DateTime<Utc>, String)],
) -> ControlResult<()> {
    if previous.phase != EconomicPhase::DisarmedFailure
        || previous.level != SizeLevel::MaxReviewed
        || previous.release_sha.as_deref() != Some(release_sha)
        || !matches!(
            binding.reason.as_str(),
            "provider_disagreement"
                | "provider_unavailable"
                | "provider_timeout"
                | "provider_rate_limited"
        )
        || binding.failure_epoch != previous.control_epoch
        || binding.failure_release != release_sha
        || binding.restore_phase != "DISARMED_EVIDENCE"
        || binding.restore_size != "MAX_REVIEWED"
        || binding.durable_sample_count != 3
        || binding.durable_samples != samples
        || samples.iter().any(|(at, _)| *at <= binding.failure_at)
    {
        return Err("provider recovery transition binding is invalid".into());
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct ProviderRecoveryRuntimeFacts {
    lanes: Vec<(String, bool, bool, u128)>,
    generic_closed: bool,
    active_attempts: i64,
    unresolved_submissions: i64,
    active_atlas: i64,
    lock_free: bool,
    current_daily_loss: u128,
    daily_loss_limit: u128,
}

fn validate_provider_recovery_runtime_facts(
    facts: &ProviderRecoveryRuntimeFacts,
) -> ControlResult<()> {
    let exact_lanes = ["aave_liquidation", "atlas_solver"];
    if facts.lanes.len() != 2
        || facts.lanes.iter().zip(exact_lanes).any(
            |((lane, armed, kill_switch, maximum), expected_lane)| {
                lane != expected_lane
                    || *armed
                    || !*kill_switch
                    || *maximum != MAXIMUM_REVIEWED_INPUT_WEI
            },
        )
        || !facts.generic_closed
        || facts.active_attempts != 0
        || facts.unresolved_submissions != 0
        || facts.active_atlas != 0
        || !facts.lock_free
        || facts.current_daily_loss >= facts.daily_loss_limit
    {
        return Err("provider recovery runtime is not fail-closed".into());
    }
    Ok(())
}

fn provider_recovery_owner_state<'a>(
    evidence: &'a Value,
    release_sha: &str,
) -> ControlResult<&'a Value> {
    let owner_state = evidence
        .get("final_state")
        .ok_or("provider recovery owner evidence is invalid")?;
    let expected_maximum = MAXIMUM_REVIEWED_INPUT_WEI.to_string();
    if evidence.get("release_sha").and_then(Value::as_str) != Some(release_sha)
        || owner_state
            .get("configuration_complete")
            .and_then(Value::as_bool)
            != Some(true)
        || owner_state
            .get("maximum_input_amount")
            .and_then(Value::as_str)
            != Some(expected_maximum.as_str())
    {
        return Err("provider recovery executor configuration diverged".into());
    }
    Ok(owner_state)
}

fn exact_release_identity() -> ControlResult<String> {
    let expected = required("PHOENIX_RELEASE_SHA")?;
    if !canonical_hex(&expected, 40) {
        return Err("release SHA is invalid".into());
    }
    for environment_name in [
        "PHOENIX_CURRENT_RELEASE_PATH",
        "PHOENIX_RELEASE_ASSETS_PATH",
    ] {
        let path = required(environment_name)?;
        let value =
            fs::read_to_string(path).map_err(|_| "protected release identity is unavailable")?;
        if value.trim() != expected {
            return Err("protected release identity diverged".into());
        }
    }
    Ok(expected)
}

async fn attempt_provider_authority_recovery(pool: &PgPool, payload: &Value) -> ControlResult<()> {
    if provider_recovery_samples(payload).is_err() {
        return Ok(());
    }
    sqlx::query(
        "UPDATE live_canary.revenue_provider_authority
         SET recovery_attempted_total=recovery_attempted_total+1, updated_at=now()
         WHERE singleton",
    )
    .execute(pool)
    .await
    .map_err(|_| "provider recovery attempt counter is unavailable")?;
    match attempt_provider_authority_recovery_inner(pool, payload).await {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = sqlx::query(
                "UPDATE live_canary.revenue_provider_authority
                 SET recovery_blocked_total=recovery_blocked_total+1,
                     recovery_status='blocked', last_block_reason='recovery_precondition_failed',
                     updated_at=now() WHERE singleton",
            )
            .execute(pool)
            .await;
            Err(error)
        }
    }
}

async fn attempt_provider_authority_recovery_inner(
    pool: &PgPool,
    payload: &Value,
) -> ControlResult<()> {
    let samples = match provider_recovery_samples(payload) {
        Ok(samples) => samples,
        Err(_) => return Ok(()),
    };
    let release_sha = exact_release_identity()?;
    if required_u128("LIVE_EXECUTOR_MAX_INPUT_AMOUNT")? != MAXIMUM_REVIEWED_INPUT_WEI {
        return Err("provider recovery maximum input is invalid".into());
    }
    let owner_evidence = runtime_preflight_from_environment().await?;
    let owner_state = provider_recovery_owner_state(&owner_evidence, &release_sha)?;
    if owner_state.get("paused").and_then(Value::as_bool) != Some(false) {
        let _ = sqlx::query(
            "UPDATE live_canary.revenue_provider_authority
             SET recovery_blocked_total = recovery_blocked_total + 1,
                 recovery_status = 'blocked', last_block_reason = 'executor_paused', updated_at = now()
             WHERE singleton",
        )
        .execute(pool)
        .await;
        return Ok(());
    }

    let maximum_gas_limit: i64 = required_u128("LIVE_EXECUTOR_MAX_GAS_LIMIT")?
        .try_into()
        .map_err(|_| "provider recovery maximum gas limit is invalid")?;
    let maximum_fee_per_gas = required_u128("LIVE_EXECUTOR_MAX_MAX_FEE_PER_GAS_WEI")?;
    let maximum_atlas_bid = required_u128("LIVE_EXECUTOR_MAX_ATLAS_BID_WEI")?;
    let daily_loss_limit = required_u128("LIVE_EXECUTOR_MAX_DAILY_LOSS_WEI")?;
    let retained_profit_floor = required_u128("LIVE_EXECUTOR_MIN_EXPECTED_PROFIT")?;

    let evidence_value = json!({
        "schema": "phoenix.revenue-provider-recovery.v1",
        "release_sha": release_sha,
        "samples": samples.iter().map(|(at, primary)| json!({
            "observed_at": at.to_rfc3339_opts(SecondsFormat::Micros, true),
            "primary_provider": primary,
            "confirmation": null,
            "quorum": 1,
        })).collect::<Vec<_>>(),
        "maximum_input_amount": MAXIMUM_REVIEWED_INPUT_WEI.to_string(),
        "owner_state": owner_state,
    });
    let evidence_bytes = serde_json::to_vec(&evidence_value)
        .map_err(|_| "provider recovery evidence serialization failed")?;
    let evidence_hash = format!("{:x}", Sha256::digest(evidence_bytes));

    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| "provider recovery transaction failed")?;
    let previous = economic_state_for_update(&mut transaction).await?;
    if previous.phase != EconomicPhase::DisarmedFailure
        || previous.level != SizeLevel::MaxReviewed
        || previous.release_sha.as_deref() != Some(release_sha.as_str())
    {
        return Ok(());
    }
    let recovery = sqlx::query(
        "SELECT failure_reason, failure_control_epoch, failure_transition_at,
                failure_release_sha, restore_phase, restore_size_level, sample_count,
                sample_1_at, sample_1_primary_provider, sample_1_confirmation_provider,
                sample_2_at, sample_2_primary_provider, sample_2_confirmation_provider,
                sample_3_at, sample_3_primary_provider, sample_3_confirmation_provider
         FROM live_canary.revenue_provider_authority WHERE singleton FOR UPDATE",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "provider recovery evidence is unavailable")?;
    let reason: String = recovery
        .try_get("failure_reason")
        .map_err(|_| "provider recovery reason is invalid")?;
    let failure_epoch: i64 = recovery
        .try_get("failure_control_epoch")
        .map_err(|_| "provider recovery epoch is invalid")?;
    let failure_at: DateTime<Utc> = recovery
        .try_get("failure_transition_at")
        .map_err(|_| "provider recovery timestamp is invalid")?;
    let failure_release: String = recovery
        .try_get("failure_release_sha")
        .map_err(|_| "provider recovery release is invalid")?;
    let restore_phase: String = recovery
        .try_get("restore_phase")
        .map_err(|_| "provider recovery phase is invalid")?;
    let restore_size: String = recovery
        .try_get("restore_size_level")
        .map_err(|_| "provider recovery size is invalid")?;
    let durable_sample_count: i16 = recovery
        .try_get("sample_count")
        .map_err(|_| "provider recovery sample count is invalid")?;
    let durable_samples = [1, 2, 3]
        .iter()
        .map(|index| {
            let confirmation = recovery
                .try_get::<Option<String>, _>(
                    format!("sample_{index}_confirmation_provider").as_str(),
                )
                .map_err(|_| "provider recovery sample confirmation is invalid")?;
            if confirmation.is_some() {
                return Err("provider recovery sample confirmation is invalid".into());
            }
            Ok((
                recovery
                    .try_get::<DateTime<Utc>, _>(format!("sample_{index}_at").as_str())
                    .map_err(|_| "provider recovery sample timestamp is invalid")?,
                recovery
                    .try_get::<String, _>(format!("sample_{index}_primary_provider").as_str())
                    .map_err(|_| "provider recovery sample primary is invalid")?,
            ))
        })
        .collect::<ControlResult<Vec<_>>>()?;
    validate_provider_recovery_transition_binding(
        &previous,
        &release_sha,
        &ProviderRecoveryTransitionBinding {
            reason,
            failure_epoch,
            failure_at,
            failure_release,
            restore_phase,
            restore_size,
            durable_sample_count,
            durable_samples,
        },
        &samples,
    )?;
    let lanes = sqlx::query(
        "SELECT lane, armed, kill_switch, maximum_input_amount::text AS maximum_input_amount
         FROM live_canary.revenue_lane_controls
         WHERE lane IN ('aave_liquidation','atlas_solver') ORDER BY lane FOR UPDATE",
    )
    .fetch_all(&mut *transaction)
    .await
    .map_err(|_| "provider recovery lane authority is unavailable")?;
    let lane_facts = lanes
        .iter()
        .map(|row| {
            Ok((
                row.try_get::<String, _>("lane")
                    .map_err(|_| "provider recovery lane is invalid")?,
                row.try_get::<bool, _>("armed")
                    .map_err(|_| "provider recovery lane is invalid")?,
                row.try_get::<bool, _>("kill_switch")
                    .map_err(|_| "provider recovery lane is invalid")?,
                row.try_get::<String, _>("maximum_input_amount")
                    .map_err(|_| "provider recovery lane is invalid")?
                    .parse::<u128>()
                    .map_err(|_| "provider recovery lane maximum is invalid")?,
            ))
        })
        .collect::<ControlResult<Vec<_>>>()?;
    let facts = sqlx::query(
        "SELECT NOT legacy.armed AND legacy.kill_switch
                    AND NOT global.armed AND global.kill_switch AND global.execution_mode = 'disarmed'
                    AND EXISTS (SELECT 1 FROM live_canary.autonomous_route_controls)
                    AND NOT EXISTS (SELECT 1 FROM live_canary.autonomous_route_controls WHERE enabled OR NOT kill_switch)
                    AND EXISTS (
                        SELECT 1 FROM live_canary.revenue_lane_controls
                        WHERE lane = 'phoenix_dex' AND NOT armed AND kill_switch
                    )
                    AS generic_closed,
                (SELECT count(*) FROM live_canary.execution_attempts WHERE status IN ('claimed','nonce_allocated','submission_unknown','pending','timed_out')) AS active_attempts,
                (SELECT count(*) FROM live_canary.execution_attempts WHERE status IN ('submission_unknown','pending','timed_out')) AS unresolved_submissions,
                (SELECT count(*) FROM live_canary.atlas_solver_requests WHERE status IN ('claimed','signed','submitted','submission_unknown')) AS active_atlas,
                lock.active_lane IS NULL AND lock.active_identity IS NULL AND lock.acquired_at IS NULL AS lock_free
         FROM live_canary.control legacy
         CROSS JOIN live_canary.autonomous_global_control global
         CROSS JOIN live_canary.global_revenue_submission_lock lock
         WHERE legacy.singleton AND global.singleton AND lock.singleton
         FOR UPDATE OF legacy, global, lock",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "provider recovery runtime proof is unavailable")?;
    let daily_loss: String = sqlx::query_scalar(
        "WITH bounds AS (SELECT date_trunc('day', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' AS start_at),
         direct AS (SELECT COALESCE(SUM(CASE WHEN net_pnl_wei < 0 THEN -net_pnl_wei ELSE 0 END),0) amount FROM live_canary.execution_outcomes,bounds WHERE recorded_at>=start_at AND recorded_at<start_at+interval '1 day'),
         atlas AS (SELECT COALESCE(SUM(i.solver_gas_limit::numeric*i.oracle_gas_price_wei),0) amount FROM live_canary.atlas_solver_requests r JOIN live_canary.atlas_auction_ingress i ON i.auction_id=r.auction_id CROSS JOIN bounds WHERE r.updated_at>=start_at AND r.updated_at<start_at+interval '1 day' AND (r.status IN ('signed','submitted','submission_unknown') OR (r.status='lost' AND (r.submission_response_hash IS NOT NULL OR r.inclusion_transaction_hash IS NOT NULL))))
         SELECT (direct.amount+atlas.amount)::text FROM direct,atlas",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "provider recovery daily loss is unavailable")?;
    let current_daily_loss = daily_loss
        .parse::<u128>()
        .map_err(|_| "provider recovery daily loss is invalid")?;
    validate_provider_recovery_runtime_facts(&ProviderRecoveryRuntimeFacts {
        lanes: lane_facts,
        generic_closed: facts
            .try_get::<bool, _>("generic_closed")
            .map_err(|_| "provider recovery runtime proof is invalid")?,
        active_attempts: facts
            .try_get::<i64, _>("active_attempts")
            .map_err(|_| "provider recovery runtime proof is invalid")?,
        unresolved_submissions: facts
            .try_get::<i64, _>("unresolved_submissions")
            .map_err(|_| "provider recovery runtime proof is invalid")?,
        active_atlas: facts
            .try_get::<i64, _>("active_atlas")
            .map_err(|_| "provider recovery runtime proof is invalid")?,
        lock_free: facts
            .try_get::<bool, _>("lock_free")
            .map_err(|_| "provider recovery runtime proof is invalid")?,
        current_daily_loss,
        daily_loss_limit,
    })?;
    let final_owner_evidence = runtime_preflight_from_environment().await?;
    let final_owner_state = provider_recovery_owner_state(&final_owner_evidence, &release_sha)?;
    if final_owner_state.get("paused").and_then(Value::as_bool) != Some(false) {
        return Err("final provider recovery executor state diverged".into());
    }
    let updated = sqlx::query(
        "UPDATE live_canary.revenue_lane_controls
         SET armed=true, kill_switch=false, maximum_input_amount=$1::numeric,
             maximum_gas_limit=$2, maximum_fee_per_gas=$3::numeric,
             maximum_atlas_bid=$4::numeric, daily_loss_limit=$5::numeric,
             retained_profit_floor=$6::numeric, disarm_reason='provider_authority_auto_recovered',
             control_epoch=control_epoch+1, updated_at=now()
         WHERE lane IN ('aave_liquidation','atlas_solver') AND NOT armed AND kill_switch",
    )
    .bind(MAXIMUM_REVIEWED_INPUT_WEI.to_string())
    .bind(maximum_gas_limit)
    .bind(maximum_fee_per_gas.to_string())
    .bind(maximum_atlas_bid.to_string())
    .bind(daily_loss_limit.to_string())
    .bind(retained_profit_floor.to_string())
    .execute(&mut *transaction)
    .await
    .map_err(|_| "provider recovery lane update failed")?;
    if updated.rows_affected() != 2 {
        return Err("provider recovery exact lane set changed concurrently".into());
    }
    let next_epoch = previous.control_epoch + 1;
    let economic = sqlx::query(
        "UPDATE live_canary.economic_control SET phase='DISARMED_EVIDENCE', control_epoch=$1,
                last_transition_reason='provider_authority_auto_recovered', updated_at=now()
         WHERE singleton AND phase='DISARMED_FAILURE' AND current_size_level='MAX_REVIEWED'
           AND release_sha=$2 AND control_epoch=$3",
    )
    .bind(next_epoch)
    .bind(&release_sha)
    .bind(previous.control_epoch)
    .execute(&mut *transaction)
    .await
    .map_err(|_| "provider recovery economic update failed")?;
    if economic.rows_affected() != 1 {
        return Err("provider recovery control changed concurrently".into());
    }
    insert_transition(
        &mut transaction,
        &previous,
        EconomicPhase::DisarmedEvidence,
        SizeLevel::MaxReviewed,
        "provider_authority_auto_recovered",
        Some(&evidence_hash),
        Some(&release_sha),
        next_epoch,
    )
    .await?;
    let audit = sqlx::query(
        "UPDATE live_canary.revenue_provider_authority
         SET exact_execution_ready=true, gate_reason='provider_authority_auto_recovered', gate_updated_at=now(),
             recovery_status='recovered', recovery_succeeded_total=recovery_succeeded_total+1,
             last_block_reason=NULL,
             recovery_evidence_hash=$1, last_recovered_at=now(), updated_at=now()
         WHERE singleton AND failure_control_epoch=$2",
    ).bind(&evidence_hash).bind(previous.control_epoch)
    .execute(&mut *transaction).await.map_err(|_| "provider recovery audit update failed")?;
    if audit.rows_affected() != 1 {
        return Err("provider recovery audit changed concurrently".into());
    }
    transaction
        .commit()
        .await
        .map_err(|_| "provider recovery commit failed")?;
    println!("REVENUE_PROVIDER_AUTHORITY_RECOVERED: release_sha={release_sha} control_epoch={next_epoch} evidence_hash={evidence_hash}");
    Ok(())
}

#[derive(Debug)]
struct ActivationCandidateAssessment {
    candidate: ActivationCandidateEvidence,
    prediction_error_bps: u16,
}

async fn eligible_activation_candidate(
    pool: &PgPool,
    state: &EconomicState,
    expected_candidate: Option<Uuid>,
    now: DateTime<Utc>,
) -> ControlResult<Option<ActivationCandidateAssessment>> {
    let route_fingerprint = state
        .route_fingerprint
        .as_deref()
        .ok_or("economic route is unavailable")?;
    let row = sqlx::query(
        r#"
SELECT candidate.candidate_id,
       candidate.candidate_hash,
       candidate.plan_hash AS candidate_plan_hash,
       candidate.state_block_number::text,
       candidate.state_block_hash,
       candidate.state_hash,
       candidate.executor_address,
       candidate.executor_code_hash,
       candidate.selected_size::text,
       candidate.predicted_gross_profit::text,
       candidate.predicted_total_cost::text,
       candidate.conservative_predicted_net_pnl::text,
       candidate.candidate_created_at,
       candidate.candidate_expires_at,
       candidate.candidate_contract,
       candidate.plan_contract,
       candidate.state_contract,
       candidate.calldata_hash,
       candidate.calldata_hex,
       result.plan_hash AS fork_plan_hash,
       result.result_hash AS fork_result_hash,
       result.simulated_net_pnl::text AS fork_simulated_net_pnl,
       result.simulated_gas_cost::text AS fork_simulated_gas_cost,
       result.simulated_at AS fork_simulated_at,
       result.plan AS fork_plan,
       jsonb_build_object(
           'result_hash', result.result_hash,
           'schema_version', result.result_schema_version,
           'plan_hash', result.plan_hash,
           'shadow_decision_id', result.shadow_decision_id::text,
           'status', result.status,
           'predicted_gross_profit', result.predicted_gross_profit::text,
           'predicted_total_cost', result.predicted_total_cost::text,
           'predicted_net_pnl', result.predicted_net_pnl::text,
           'simulated_gross_profit', result.simulated_gross_profit::text,
           'simulated_gas_cost', result.simulated_gas_cost::text,
           'simulated_balance_delta', result.simulated_balance_delta::text,
           'simulated_net_pnl', result.simulated_net_pnl::text,
           'prediction_error', result.prediction_error::text,
           'gas_estimate', result.gas_estimate::bigint,
           'gas_used', result.gas_used::bigint,
           'model_version', result.model_version,
           'policy_version', result.policy_version,
           'fork', jsonb_build_object(
               'chain_id', result.fork_chain_id,
               'fork_block', jsonb_build_object(
                   'number', result.fork_block_number::bigint,
                   'hash', result.fork_block_hash
               ),
               'fork_instance_hash', result.fork_instance_hash,
               'local_block', jsonb_build_object(
                   'number', result.local_block_number::bigint,
                   'hash', result.local_block_hash
               )
           ),
           'simulated_at', result.simulated_at,
           'revert_reason', result.revert_reason,
           'evidence', result.evidence,
           'fork_only', result.fork_only,
           'shadow_only', result.shadow_only,
           'live_execution', result.live_execution,
           'execution_eligible', result.execution_eligible,
           'execution_request_created', result.execution_request_created,
           'public_broadcast', result.public_broadcast,
           'signer_used', result.signer_used
       ) AS fork_result
FROM live_canary.autonomous_candidates candidate
JOIN public.shadow_profitability_facts fact
  ON fact.source_event_identity = candidate.origin_event_id
 AND fact.route_fingerprint = candidate.route_fingerprint
JOIN public.fork_simulation_results result
  ON result.shadow_decision_id = fact.shadow_decision_id
WHERE candidate.route_fingerprint = $1
  AND candidate.route_policy_hash = $6
  AND candidate.status = 'materialized'
  AND candidate.selected_size = $2::numeric
  AND candidate.candidate_created_at >= $3
  AND candidate.candidate_expires_at > $4
  AND candidate.conservative_predicted_net_pnl > 0
  AND candidate.risk_snapshot_hash = repeat('0', 64)
  AND candidate.submission_quote_hash = repeat('0', 64)
  AND fact.evidence_completeness_status = 'complete'
  AND fact.disposition = 'accepted'
  AND fact.primary_profitability_status = 'meets_minimum'
  AND fact.expected_net_pnl > 0
  AND fact.conservative_net_pnl > 0
  AND fact.verification_status = 'agreed'
  AND fact.independent_verification_status = 'agreed'
  AND fact.agreement_state = 'agreed'
  AND fact.secondary_provider_id IS NOT NULL
  AND fact.secondary_provider_id <> fact.primary_provider_id
  AND fact.opportunity_expires_at > $4
  AND result.status = 'passed'
  AND result.simulated_net_pnl > 0
  AND result.simulated_at >= $3
  AND result.plan #>> '{route,route_fingerprint}' = candidate.route_fingerprint
  AND result.plan ->> 'source_event_identity' = candidate.origin_event_id
  AND result.plan ->> 'input_amount' = candidate.selected_size::text
  AND result.plan ->> 'calldata_hash' = candidate.calldata_hash
  AND result.plan ->> 'target_contract' = candidate.executor_address
  AND result.plan ->> 'target_code_hash' = candidate.executor_code_hash
  AND result.plan #>> '{pinned_block,number}' = candidate.state_block_number::text
  AND result.plan #>> '{pinned_block,hash}' = candidate.state_block_hash
  AND ($5::uuid IS NULL OR candidate.candidate_id = $5)
ORDER BY candidate.candidate_created_at DESC, result.simulated_at DESC
LIMIT 1
"#,
    )
    .bind(route_fingerprint)
    .bind(SizeLevel::Min.amount_wei().to_string())
    .bind(state.updated_at)
    .bind(now)
    .bind(expected_candidate)
    .bind(
        state
            .route_policy_hash
            .as_deref()
            .ok_or("route policy identity is unavailable")?,
    )
    .fetch_optional(pool)
    .await
    .map_err(|_| "eligible activation candidate inspection failed")?;
    let Some(row) = row else {
        return Ok(None);
    };

    let candidate_contract = row
        .try_get::<sqlx::types::Json<Value>, _>("candidate_contract")
        .map_err(|_| "candidate contract is invalid")?
        .0;
    verify_hash(
        &candidate_contract,
        "candidate_hash",
        "autonomous-candidate",
        "phoenix.autonomous-candidate.v1",
    )?;
    let candidate_hash: String = row
        .try_get("candidate_hash")
        .map_err(|_| "candidate hash is invalid")?;
    if candidate_contract
        .get("candidate_hash")
        .and_then(Value::as_str)
        != Some(candidate_hash.as_str())
    {
        return Err("candidate hash is invalid".into());
    }

    let candidate_plan = row
        .try_get::<sqlx::types::Json<Value>, _>("plan_contract")
        .map_err(|_| "candidate plan is invalid")?
        .0;
    let candidate_plan_schema = value_text(&candidate_plan, "schema_version")?;
    if candidate_plan_schema != "phoenix.hunter-live-plan.v1"
        || candidate_plan.get("mode").and_then(Value::as_str) != Some("live")
        || candidate_plan
            .get("execution_eligible")
            .and_then(Value::as_bool)
            != Some(true)
        || candidate_plan.get("shadow_only").and_then(Value::as_bool) != Some(false)
        || candidate_plan.get("signer_used").and_then(Value::as_bool) != Some(false)
        || candidate_plan
            .get("public_broadcast")
            .and_then(Value::as_bool)
            != Some(false)
    {
        return Err("candidate plan is not eligible LIVE evidence".into());
    }
    let candidate_plan_hash = canonical_domain_hash(candidate_plan_schema, &candidate_plan)
        .map_err(|_| "candidate plan hash is invalid")?;
    let persisted_candidate_plan_hash: String = row
        .try_get("candidate_plan_hash")
        .map_err(|_| "candidate plan hash is invalid")?;
    if candidate_plan_hash != persisted_candidate_plan_hash
        || candidate_contract.get("plan_hash").and_then(Value::as_str)
            != Some(candidate_plan_hash.as_str())
    {
        return Err("candidate plan hash is invalid".into());
    }

    let calldata_hex: String = row
        .try_get("calldata_hex")
        .map_err(|_| "candidate calldata is invalid")?;
    let calldata = calldata_hex
        .strip_prefix("0x")
        .and_then(|encoded| hex::decode(encoded).ok())
        .filter(|value| !value.is_empty())
        .ok_or("candidate calldata is invalid")?;
    let calldata_hash: String = row
        .try_get("calldata_hash")
        .map_err(|_| "candidate calldata is invalid")?;
    if hex::encode(Sha256::digest(calldata)) != calldata_hash {
        return Err("candidate calldata hash is invalid".into());
    }

    let state_contract = row
        .try_get::<sqlx::types::Json<Value>, _>("state_contract")
        .map_err(|_| "candidate state evidence is invalid")?
        .0;
    let hunter_state: HunterStateResponse = serde_json::from_value(state_contract)
        .map_err(|_| "candidate state evidence is invalid")?;
    let state_block_number = row_u64_text(&row, "state_block_number")?;
    let state_block_hash: String = row
        .try_get("state_block_hash")
        .map_err(|_| "candidate state evidence is invalid")?;
    if hunter_state.schema_version != HUNTER_STATE_RESPONSE_SCHEMA
        || hunter_state.chain_id != 42_161
        || hunter_state.block_number != state_block_number
        || hunter_state.block_hash != state_block_hash
        || hunter_state.agreements.is_empty()
    {
        return Err("candidate state evidence is invalid".into());
    }
    for agreement in &hunter_state.agreements {
        let agreed = agreement
            .agreed()
            .map_err(|_| "candidate providers do not agree")?;
        if agreed.block_number != state_block_number || agreed.block_hash != state_block_hash {
            return Err("candidate state evidence is invalid".into());
        }
    }

    let fork_plan: UnsignedTransactionPlan = serde_json::from_value(
        row.try_get::<sqlx::types::Json<Value>, _>("fork_plan")
            .map_err(|_| "fork plan is invalid")?
            .0,
    )
    .map_err(|_| "fork plan is invalid")?;
    let fork_plan_hash = fork_plan
        .canonical_hash()
        .map_err(|_| "fork plan hash is invalid")?;
    let persisted_fork_plan_hash: String = row
        .try_get("fork_plan_hash")
        .map_err(|_| "fork plan hash is invalid")?;
    if fork_plan_hash != persisted_fork_plan_hash {
        return Err("fork plan hash is invalid".into());
    }
    let fork_result: CounterfactualResult = serde_json::from_value(
        row.try_get::<sqlx::types::Json<Value>, _>("fork_result")
            .map_err(|_| "fork result is invalid")?
            .0,
    )
    .map_err(|_| "fork result is invalid")?;
    fork_result
        .validate_plan_binding(&fork_plan)
        .map_err(|_| "fork result binding is invalid")?;
    let executor_address: String = row
        .try_get("executor_address")
        .map_err(|_| "candidate executor identity is invalid")?;
    let executor_code_hash: String = row
        .try_get("executor_code_hash")
        .map_err(|_| "candidate executor identity is invalid")?;
    let state_hash: String = row
        .try_get("state_hash")
        .map_err(|_| "candidate state evidence is invalid")?;
    if fork_result.body.status != SimulationStatus::Passed
        || fork_plan.source_event_identity
            != candidate_contract
                .get("origin_event_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
        || fork_plan.route.route_fingerprint != route_fingerprint
        || fork_plan.calldata_hash != calldata_hash
        || fork_plan.target_contract != executor_address
        || fork_plan.target_code_hash != executor_code_hash
        || fork_plan.pinned_block.number != state_block_number
        || fork_plan.pinned_block.hash != state_block_hash
        || fork_plan.primary_state_hash != state_hash
    {
        return Err("candidate and fork evidence do not bind".into());
    }

    let predicted_net = fork_result
        .body
        .predicted_net_pnl
        .parse::<i128>()
        .map_err(|_| "fork economics are invalid")?;
    let simulated_net = fork_result
        .body
        .simulated_net_pnl
        .as_deref()
        .and_then(|value| value.parse::<i128>().ok())
        .ok_or("fork economics are invalid")?;
    let prediction_error_bps = if predicted_net == 0 {
        u16::MAX
    } else {
        u16::try_from(
            simulated_net
                .saturating_sub(predicted_net)
                .unsigned_abs()
                .saturating_mul(10_000)
                .checked_div(predicted_net.unsigned_abs())
                .unwrap_or(u128::MAX)
                .min(u128::from(u16::MAX)),
        )
        .map_err(|_| "fork economics are invalid")?
    };
    let candidate_created_at: DateTime<Utc> = row
        .try_get("candidate_created_at")
        .map_err(|_| "candidate clock evidence is invalid")?;
    let candidate = ActivationCandidateEvidence {
        candidate_id: row
            .try_get("candidate_id")
            .map_err(|_| "candidate identity is invalid")?,
        candidate_hash,
        candidate_plan_hash,
        fork_plan_hash,
        fork_result_hash: row
            .try_get("fork_result_hash")
            .map_err(|_| "fork result hash is invalid")?,
        route_fingerprint: route_fingerprint.to_string(),
        route_policy_hash: state
            .route_policy_hash
            .clone()
            .ok_or("route policy identity is unavailable")?,
        state_block_number,
        state_block_hash,
        state_hash,
        executor_address,
        executor_code_hash,
        selected_size_wei: row_u128_text(&row, "selected_size")?,
        predicted_gross_profit_wei: row_u128_text(&row, "predicted_gross_profit")?,
        predicted_total_cost_wei: row_u128_text(&row, "predicted_total_cost")?,
        conservative_predicted_net_pnl_wei: row_i128_text(&row, "conservative_predicted_net_pnl")?,
        fork_simulated_net_pnl_wei: row_i128_text(&row, "fork_simulated_net_pnl")?,
        fork_simulated_gas_cost_wei: row_u128_text(&row, "fork_simulated_gas_cost")?,
        candidate_created_at,
        candidate_expires_at: row
            .try_get("candidate_expires_at")
            .map_err(|_| "candidate clock evidence is invalid")?,
        fork_simulated_at: row
            .try_get("fork_simulated_at")
            .map_err(|_| "fork clock evidence is invalid")?,
    };
    candidate
        .validate(now)
        .map_err(|_| "activation candidate is stale or invalid")?;
    Ok(Some(ActivationCandidateAssessment {
        candidate,
        prediction_error_bps,
    }))
}

async fn collect_activation_request(
    pool: &PgPool,
    expected: Option<&ActivationRequest>,
) -> ControlResult<Option<ActivationRequest>> {
    require_schema(pool).await?;
    let state = economic_state(pool).await?;
    if state.phase != EconomicPhase::DisarmedEvidence {
        return Ok(None);
    }
    let release_sha = state
        .release_sha
        .as_deref()
        .ok_or("economic release identity is unavailable")?;
    if required("PHOENIX_RELEASE_SHA")? != release_sha {
        return Err("economic release identity does not match the runtime".into());
    }
    let engine_image_digest = state
        .engine_image_digest
        .as_deref()
        .ok_or("Engine image identity is unavailable")?;
    if image_digest(&required("PHOENIX_ENGINE_IMAGE")?)? != engine_image_digest {
        return Err("Engine image identity does not match the runtime".into());
    }
    let database_now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(pool)
        .await
        .map_err(|_| "database clock is unavailable")?;
    let mut policies = reviewed_policy_values()?;
    policies.sort_by_key(|policy| {
        policy.get("route_fingerprint").and_then(Value::as_str)
            != state.route_fingerprint.as_deref()
    });
    let mut selected = None;
    for policy in policies {
        let fingerprint = value_text(&policy, "route_fingerprint")?;
        if expected
            .map(|request| request.candidate.route_fingerprint.as_str() != fingerprint)
            .unwrap_or(false)
        {
            continue;
        }
        let mut route_state = state.clone();
        route_state.route_fingerprint = Some(fingerprint.to_string());
        route_state.route_policy_hash = Some(value_text(&policy, "policy_hash")?.to_string());
        route_state
            .risk_policy_hash
            .clone_from(&route_state.route_policy_hash);
        if let Some(assessment) = eligible_activation_candidate(
            pool,
            &route_state,
            expected.map(|request| request.candidate.candidate_id),
            database_now,
        )
        .await?
        {
            selected = Some((route_state, assessment));
            break;
        }
    }
    let Some((state, assessment)) = selected else {
        return Ok(None);
    };
    let route_fingerprint = state
        .route_fingerprint
        .as_deref()
        .ok_or("economic route is unavailable")?;

    let controls = sqlx::query(
        r#"
WITH observation AS (
    SELECT count(*) AS total,
           count(*) FILTER (
               WHERE classification NOT IN (
                   'malformed_internal_event', 'unsupported_schema',
                   'terminal_integrity_failure'
               )
           ) AS supported,
           count(*) FILTER (
               WHERE classification = 'terminal_integrity_failure'
           ) AS fatal
    FROM public.shadow_engine_classifications
    WHERE classified_at >= $2
),
quarantine AS (
    SELECT max(classified_at) FILTER (
               WHERE classification IN (
                   'malformed_internal_event', 'unsupported_schema',
                   'dependency_exhausted', 'terminal_integrity_failure'
               )
           ) AS latest
    FROM public.shadow_engine_classifications
    WHERE classified_at >= $2
),
outbox AS (
    SELECT count(*) FILTER (WHERE published_at IS NULL) AS pending,
           count(*) FILTER (
               WHERE published_at IS NULL
                 AND created_at < clock_timestamp() - interval '60 seconds'
           ) AS stale
    FROM public.engine_outbox
),
bounds AS (
    SELECT date_trunc('day', clock_timestamp() AT TIME ZONE 'UTC')
           AT TIME ZONE 'UTC' AS start_at
),
direct_loss AS (
    SELECT COALESCE(sum(
               CASE WHEN outcome.net_pnl_wei < 0 THEN -outcome.net_pnl_wei ELSE 0 END
           ), 0) AS amount
    FROM live_canary.execution_outcomes outcome
    CROSS JOIN bounds
    WHERE outcome.recorded_at >= bounds.start_at
      AND outcome.recorded_at < bounds.start_at + interval '1 day'
),
atlas_loss AS (
    SELECT COALESCE(sum(
               ingress.solver_gas_limit::numeric * ingress.oracle_gas_price_wei
           ), 0) AS amount
    FROM live_canary.atlas_solver_requests request
    JOIN live_canary.atlas_auction_ingress ingress
      ON ingress.auction_id = request.auction_id
    CROSS JOIN bounds
    WHERE request.updated_at >= bounds.start_at
      AND request.updated_at < bounds.start_at + interval '1 day'
      AND (
        request.status IN ('signed', 'submitted', 'submission_unknown')
        OR (
          request.status = 'lost'
          AND (
            request.submission_response_hash IS NOT NULL
            OR request.inclusion_transaction_hash IS NOT NULL
          )
        )
      )
),
loss AS (
    SELECT (direct_loss.amount + atlas_loss.amount)::text AS current_daily_loss,
           global.daily_loss_limit::text AS daily_loss_limit
    FROM live_canary.autonomous_global_control global
    CROSS JOIN direct_loss
    CROSS JOIN atlas_loss
    WHERE global.singleton
)
SELECT NOT legacy.armed AND legacy.kill_switch
           AND NOT global.armed AND global.kill_switch
           AND global.execution_mode = 'disarmed'
           AND NOT route.enabled AND route.kill_switch AS controls_closed,
       global.control_epoch AS global_epoch,
       route.control_epoch AS route_epoch,
       observation.total,
       observation.supported,
       observation.fatal,
       quarantine.latest IS NULL OR EXISTS (
           SELECT 1 FROM public.shadow_engine_classifications later
           WHERE later.classified_at > quarantine.latest
             AND later.classification NOT IN (
                 'malformed_internal_event', 'unsupported_schema',
                 'terminal_integrity_failure'
             )
       ) AS quarantine_progress,
       outbox.pending,
       outbox.stale,
       (SELECT count(*) FROM live_canary.execution_requests request
        WHERE request.created_at >= $2) AS execution_requests,
       (SELECT count(*) FROM live_canary.execution_attempts attempt
        WHERE attempt.status IN (
            'claimed', 'nonce_allocated', 'submission_unknown', 'pending', 'timed_out'
        )) AS active_attempts,
       (SELECT count(*) FROM live_canary.execution_attempts attempt
        WHERE attempt.status IN ('submission_unknown', 'pending', 'timed_out')
       ) AS unresolved_submissions,
       (SELECT count(*) FROM public.shadow_profitability_facts fact
        WHERE fact.evaluated_at >= $2
          AND fact.primary_profitability_status = 'meets_minimum'
          AND fact.independent_verification_status = 'disagreed'
       ) AS rpc_disagreements,
       loss.current_daily_loss,
       loss.daily_loss_limit
FROM live_canary.control legacy
CROSS JOIN live_canary.autonomous_global_control global
JOIN live_canary.autonomous_route_controls route
  ON route.route_fingerprint = $1
CROSS JOIN observation
CROSS JOIN quarantine
CROSS JOIN outbox
CROSS JOIN loss
WHERE legacy.singleton AND global.singleton
"#,
    )
    .bind(route_fingerprint)
    .bind(state.updated_at)
    .fetch_one(pool)
    .await
    .map_err(|_| "activation readiness evidence is unavailable")?;
    if !controls
        .try_get::<bool, _>("controls_closed")
        .map_err(|_| "activation controls are invalid")?
        || nonnegative_u64(&controls, "active_attempts")? != 0
        || nonnegative_u64(&controls, "unresolved_submissions")? != 0
    {
        return Ok(None);
    }
    let total = nonnegative_u64(&controls, "total")?;
    let supported = nonnegative_u64(&controls, "supported")?;
    let valid_acceptance_bps = u16::try_from(
        supported
            .saturating_mul(10_000)
            .checked_div(total)
            .unwrap_or(0),
    )
    .unwrap_or(0);
    let candidate_age_ms = nonnegative_milliseconds(
        database_now.signed_duration_since(assessment.candidate.candidate_created_at),
    )?;
    let evidence = EvidenceGate {
        supported_observations: supported,
        valid_acceptance_bps,
        process_fatal_integrity_exits: nonnegative_u64(&controls, "fatal")?,
        quarantine_progress_proven: controls
            .try_get("quarantine_progress")
            .map_err(|_| "quarantine evidence is invalid")?,
        consumer_pending_bounded: nonnegative_u64(&controls, "pending")? <= 1_000,
        ack_pending_bounded: nonnegative_u64(&controls, "stale")? == 0,
        stale_outbox_rows: nonnegative_u64(&controls, "stale")?,
        primary_rpc_healthy: true,
        maximum_state_age_blocks: 0,
        maximum_quote_age_ms: candidate_age_ms,
        maximum_candidate_age_ms: candidate_age_ms,
        fork_attempts: 1,
        fork_passes: 1,
        prediction_error_bps: assessment.prediction_error_bps,
        fork_skips: 0,
        execution_requests: nonnegative_u64(&controls, "execution_requests")?,
        active_attempts: nonnegative_u64(&controls, "active_attempts")?,
        positive_exact_candidates: 1,
    };
    if evidence.validate().is_err() {
        return Ok(None);
    }

    let reserve = wallet_gas_reserve().await?;
    let current_daily_loss = row_u128_text(&controls, "current_daily_loss")?;
    let daily_loss_limit = row_u128_text(&controls, "daily_loss_limit")?;
    if reserve <= state.gas_reserve_floor_wei || current_daily_loss >= daily_loss_limit {
        return Ok(None);
    }
    let owner_evidence = configured_preflight_from_environment().await?;
    if owner_evidence.get("status").and_then(Value::as_str) != Some("ready-paused") {
        return Ok(None);
    }
    let contract_identity_hash =
        canonical_domain_hash("phoenix.executor-contract-identity.v1", &owner_evidence)
            .map_err(|_| "contract identity evidence is invalid")?;
    let expires_at = database_now + ChronoDuration::seconds(ACTIVATION_REQUEST_TTL_SECONDS);
    let binding = ReadinessBinding {
        release_sha: release_sha.to_string(),
        engine_image_digest: engine_image_digest.to_string(),
        route_fingerprint: route_fingerprint.to_string(),
        route_universe_hash: state
            .route_universe_hash
            .clone()
            .ok_or("route universe identity is unavailable")?,
        route_policy_hash: state
            .route_policy_hash
            .clone()
            .ok_or("route policy identity is unavailable")?,
        risk_policy_hash: state
            .risk_policy_hash
            .clone()
            .ok_or("risk policy identity is unavailable")?,
        economic_control_epoch: u64::try_from(state.control_epoch)
            .map_err(|_| "economic control epoch is invalid")?,
        global_control_epoch: u64::try_from(
            controls
                .try_get::<i64, _>("global_epoch")
                .map_err(|_| "global control epoch is invalid")?,
        )
        .map_err(|_| "global control epoch is invalid")?,
        route_control_epoch: u64::try_from(
            controls
                .try_get::<i64, _>("route_epoch")
                .map_err(|_| "route control epoch is invalid")?,
        )
        .map_err(|_| "route control epoch is invalid")?,
        executor_code_hash: state
            .executor_code_hash
            .clone()
            .ok_or("executor code identity is unavailable")?,
        contract_identity_hash,
        wallet_gas_reserve_wei: reserve,
        gas_reserve_floor_wei: state.gas_reserve_floor_wei,
        current_daily_loss_wei: current_daily_loss,
        daily_loss_limit_wei: daily_loss_limit,
        observed_from: state.updated_at,
        observed_until: database_now - ChronoDuration::microseconds(1),
        created_at: database_now,
        expires_at,
        candidate_evidence_hashes: vec![
            assessment.candidate.candidate_hash.clone(),
            assessment.candidate.fork_result_hash.clone(),
        ],
    };
    let mut request = ActivationRequest {
        schema_version: ACTIVATION_REQUEST_SCHEMA.to_string(),
        request_id: expected
            .map(|request| request.request_id)
            .unwrap_or_else(Uuid::new_v4),
        binding,
        evidence,
        candidate: assessment.candidate,
        created_at: database_now,
        expires_at,
        request_hash: "0".repeat(64),
    };
    request
        .seal()
        .map_err(|_| "activation request hash failed")?;
    request
        .validate(database_now)
        .map_err(|_| "activation request is invalid")?;
    Ok(Some(request))
}

fn row_u64_text(row: &sqlx::postgres::PgRow, name: &str) -> ControlResult<u64> {
    row.try_get::<String, _>(name)
        .map_err(|_| "numeric evidence is invalid")?
        .parse()
        .map_err(|_| "numeric evidence is invalid".into())
}

fn row_u128_text(row: &sqlx::postgres::PgRow, name: &str) -> ControlResult<u128> {
    row.try_get::<String, _>(name)
        .map_err(|_| "numeric evidence is invalid")?
        .parse()
        .map_err(|_| "numeric evidence is invalid".into())
}

fn row_i128_text(row: &sqlx::postgres::PgRow, name: &str) -> ControlResult<i128> {
    row.try_get::<String, _>(name)
        .map_err(|_| "numeric evidence is invalid")?
        .parse()
        .map_err(|_| "numeric evidence is invalid".into())
}

fn nonnegative_u64(row: &sqlx::postgres::PgRow, name: &str) -> ControlResult<u64> {
    let value = row
        .try_get::<i64, _>(name)
        .map_err(|_| "numeric evidence is invalid")?;
    u64::try_from(value).map_err(|_| "numeric evidence is invalid".into())
}

async fn evaluate_economic_control(pool: &PgPool) -> ControlResult<()> {
    require_schema(pool).await?;
    let reserve = wallet_gas_reserve().await?;
    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| "database transaction failed")?;
    let previous = economic_state_for_update(&mut transaction).await?;
    if !previous.phase.is_live() {
        transaction
            .commit()
            .await
            .map_err(|_| "economic supervision commit failed")?;
        println!(
            "ECONOMIC_SUPERVISION_IDLE: phase={}",
            previous.phase.as_str()
        );
        return Ok(());
    }
    let authorization_valid: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1
            FROM live_canary.automation_authorizations authorization
            WHERE authorization.authorization_id = $1
              AND authorization.consumed_at IS NOT NULL
              AND authorization.expires_at > now()
              AND authorization.one_transaction_at_a_time
              AND authorization.reviewed_ladder_only
              AND authorization.automatic_disarm_required
              AND authorization.maximum_reviewed_input_wei = $2::numeric
         )",
    )
    .bind(previous.authorization_id)
    .bind(MAXIMUM_REVIEWED_INPUT_WEI.to_string())
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "automation authorization inspection failed")?;
    if !authorization_valid {
        route_failure(
            &mut transaction,
            &previous,
            "automation_authorization_expired",
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(|_| "authorization disarm commit failed")?;
        println!("ECONOMIC_ROUTE_DISARMED: reason=automation_authorization_expired");
        return Ok(());
    }

    let rows = sqlx::query(
        "SELECT outcome.realized_business_net_pnl::text AS net_pnl,
                outcome.receipt_status,
                outcome.prediction_error::text AS prediction_error,
                outcome.predicted_net_pnl::text AS predicted_net_pnl,
                candidate.candidate_created_at,
                outcome.submitted_at,
                (candidate.submission_quote_contract->>'quote_created_at')::timestamptz
                    AS quote_created_at,
                outcome.nonce::text AS nonce
         FROM live_canary.autonomous_outcome_attributions outcome
         JOIN live_canary.autonomous_candidates candidate
           ON candidate.candidate_id = outcome.candidate_id
         WHERE outcome.input_size_level = $1
           AND candidate.route_fingerprint = $2
         ORDER BY outcome.attributed_at, outcome.candidate_id",
    )
    .bind(previous.level.as_str())
    .bind(
        previous
            .route_fingerprint
            .as_deref()
            .ok_or("economic route is unavailable")?,
    )
    .fetch_all(&mut *transaction)
    .await
    .map_err(|_| "promotion outcome evidence is unavailable")?;
    let reconciled_outcomes =
        u64::try_from(rows.len()).map_err(|_| "promotion evidence is invalid")?;
    let mut realized_net = 0_i128;
    let mut successful = 0_u64;
    let mut absolute_prediction_error = 0_u128;
    let mut absolute_predicted_net = 0_u128;
    let mut maximum_quote_age_ms = 0_u64;
    let mut maximum_candidate_age_ms = 0_u64;
    let mut nonces = Vec::new();
    for row in &rows {
        let net = row
            .try_get::<String, _>("net_pnl")
            .map_err(|_| "promotion evidence is invalid")?
            .parse::<i128>()
            .map_err(|_| "promotion evidence is invalid")?;
        realized_net = realized_net
            .checked_add(net)
            .ok_or("promotion evidence overflow")?;
        if row
            .try_get::<Option<i16>, _>("receipt_status")
            .map_err(|_| "promotion evidence is invalid")?
            == Some(1)
        {
            successful += 1;
        }
        let error = row
            .try_get::<Option<String>, _>("prediction_error")
            .map_err(|_| "promotion evidence is invalid")?
            .and_then(|value| value.parse::<i128>().ok())
            .ok_or("prediction evidence is incomplete")?;
        let predicted = row
            .try_get::<Option<String>, _>("predicted_net_pnl")
            .map_err(|_| "promotion evidence is invalid")?
            .and_then(|value| value.parse::<i128>().ok())
            .ok_or("prediction evidence is incomplete")?;
        absolute_prediction_error = absolute_prediction_error
            .checked_add(error.unsigned_abs())
            .ok_or("promotion evidence overflow")?;
        absolute_predicted_net = absolute_predicted_net
            .checked_add(predicted.unsigned_abs())
            .ok_or("promotion evidence overflow")?;
        let candidate_created_at: chrono::DateTime<Utc> = row
            .try_get("candidate_created_at")
            .map_err(|_| "promotion evidence is invalid")?;
        let submitted_at: chrono::DateTime<Utc> = row
            .try_get::<Option<chrono::DateTime<Utc>>, _>("submitted_at")
            .map_err(|_| "promotion evidence is invalid")?
            .ok_or("submitted outcome timestamp is missing")?;
        let quote_created_at: chrono::DateTime<Utc> = row
            .try_get("quote_created_at")
            .map_err(|_| "promotion evidence is invalid")?;
        maximum_quote_age_ms = maximum_quote_age_ms.max(nonnegative_milliseconds(
            submitted_at.signed_duration_since(quote_created_at),
        )?);
        maximum_candidate_age_ms = maximum_candidate_age_ms.max(nonnegative_milliseconds(
            submitted_at.signed_duration_since(candidate_created_at),
        )?);
        nonces.push(
            row.try_get::<Option<String>, _>("nonce")
                .map_err(|_| "promotion evidence is invalid")?
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or("nonce evidence is incomplete")?,
        );
    }
    nonces.sort_unstable();
    nonces.dedup();
    let nonce_gaps = nonces
        .windows(2)
        .map(|pair| pair[1].saturating_sub(pair[0]).saturating_sub(1))
        .sum();
    let prediction_error_bps = if absolute_predicted_net == 0 {
        u16::MAX
    } else {
        u16::try_from(
            absolute_prediction_error
                .saturating_mul(10_000)
                .checked_div(absolute_predicted_net)
                .unwrap_or(u128::MAX)
                .min(u128::from(u16::MAX)),
        )
        .map_err(|_| "prediction evidence is invalid")?
    };
    let (fork_attempts, fork_passes): (i64, i64) = sqlx::query_as(
        "SELECT count(result.result_hash),
                count(result.result_hash) FILTER (WHERE result.status = 'passed')
         FROM live_canary.autonomous_candidates candidate
         LEFT JOIN public.fork_simulation_results result
           ON result.plan_hash = candidate.plan_hash
         WHERE candidate.route_fingerprint = $1
           AND candidate.selected_size = $2::numeric",
    )
    .bind(
        previous
            .route_fingerprint
            .as_deref()
            .ok_or("economic route is unavailable")?,
    )
    .bind(previous.level.amount_wei().to_string())
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "fork promotion evidence is unavailable")?;
    let rpc_disagreements: i64 = sqlx::query_scalar(
        "SELECT count(*)
         FROM live_canary.autonomous_candidates candidate
         WHERE candidate.route_fingerprint = $1
           AND candidate.selected_size = $2::numeric
           AND candidate.status IN (
               'submitted', 'confirmed_profitable', 'confirmed_unprofitable', 'reverted'
           )
           AND (
               candidate.plan_contract #>> '{verification,agreement_state}' IS DISTINCT FROM 'agreed'
               OR candidate.plan_contract #>> '{verification,independent_verification_status}'
                  IS DISTINCT FROM 'agreed'
           )",
    )
    .bind(
        previous
            .route_fingerprint
            .as_deref()
            .ok_or("economic route is unavailable")?,
    )
    .bind(previous.level.amount_wei().to_string())
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "RPC promotion evidence is unavailable")?;
    let unknown_submissions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM live_canary.autonomous_candidates
         WHERE route_fingerprint = $1 AND status = 'submission_unknown'",
    )
    .bind(
        previous
            .route_fingerprint
            .as_deref()
            .ok_or("economic route is unavailable")?,
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "unknown-submission evidence is unavailable")?;
    let control_violations: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM live_canary.autonomous_candidates
         WHERE route_fingerprint = $1
           AND status IN (
               'request_materialized', 'claimed', 'signed', 'submitted',
               'confirmed_profitable', 'confirmed_unprofitable'
           )
           AND selected_size <> $2::numeric",
    )
    .bind(
        previous
            .route_fingerprint
            .as_deref()
            .ok_or("economic route is unavailable")?,
    )
    .bind(previous.level.amount_wei().to_string())
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "control evidence is unavailable")?;
    let unreconciled_receipts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM live_canary.execution_attempts
         WHERE status IN ('pending', 'timed_out', 'submission_unknown')
           AND coalesce(submitted_at, claimed_at) < now() - interval '180 seconds'",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "receipt evidence is unavailable")?;
    let fatal_integrity: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM live_canary.autonomous_candidates
         WHERE route_fingerprint = $1 AND status = 'integrity_failure'",
    )
    .bind(
        previous
            .route_fingerprint
            .as_deref()
            .ok_or("economic route is unavailable")?,
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "integrity evidence is unavailable")?;
    let global: (String, String) = sqlx::query_as(
        "WITH bounds AS (
            SELECT date_trunc('day', now() AT TIME ZONE 'UTC')
                   AT TIME ZONE 'UTC' AS start_at
         ), direct_loss AS (
            SELECT COALESCE(sum(
                       CASE WHEN outcome.net_pnl_wei < 0 THEN -outcome.net_pnl_wei ELSE 0 END
                   ), 0) AS amount
            FROM live_canary.execution_outcomes outcome
            CROSS JOIN bounds
            WHERE outcome.recorded_at >= bounds.start_at
              AND outcome.recorded_at < bounds.start_at + interval '1 day'
         ), atlas_loss AS (
            SELECT COALESCE(sum(
                       ingress.solver_gas_limit::numeric * ingress.oracle_gas_price_wei
                   ), 0) AS amount
            FROM live_canary.atlas_solver_requests request
            JOIN live_canary.atlas_auction_ingress ingress
              ON ingress.auction_id = request.auction_id
            CROSS JOIN bounds
            WHERE request.updated_at >= bounds.start_at
              AND request.updated_at < bounds.start_at + interval '1 day'
              AND (
                request.status IN ('signed', 'submitted', 'submission_unknown')
                OR (
                  request.status = 'lost'
                  AND (
                    request.submission_response_hash IS NOT NULL
                    OR request.inclusion_transaction_hash IS NOT NULL
                  )
                )
              )
         )
         SELECT (direct_loss.amount + atlas_loss.amount)::text,
                control.daily_loss_limit::text
         FROM live_canary.autonomous_global_control control
         CROSS JOIN direct_loss
         CROSS JOIN atlas_loss
         WHERE control.singleton",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "daily-loss evidence is unavailable")?;
    let daily_loss = global
        .0
        .parse::<u128>()
        .map_err(|_| "daily-loss evidence is invalid")?;
    let daily_limit = global
        .1
        .parse::<u128>()
        .map_err(|_| "daily-loss evidence is invalid")?;
    let consecutive_losses = rows
        .iter()
        .rev()
        .map(|row| row.try_get::<String, _>("net_pnl"))
        .take_while(|value| {
            value
                .as_ref()
                .ok()
                .and_then(|value| value.parse::<i128>().ok())
                .is_some_and(|value| value < 0)
        })
        .count();
    let evidence = PromotionEvidence {
        reconciled_outcomes,
        aggregate_realized_net_pnl_wei: realized_net,
        successful_outcomes: successful,
        fork_attempts: u64::try_from(fork_attempts)
            .map_err(|_| "fork promotion evidence is invalid")?,
        fork_passes: u64::try_from(fork_passes)
            .map_err(|_| "fork promotion evidence is invalid")?,
        rpc_disagreements: u64::try_from(rpc_disagreements)
            .map_err(|_| "RPC promotion evidence is invalid")?,
        unknown_submissions: u64::try_from(unknown_submissions)
            .map_err(|_| "unknown-submission evidence is invalid")?,
        duplicate_submissions: 0,
        nonce_gaps,
        control_violations: u64::try_from(control_violations)
            .map_err(|_| "control evidence is invalid")?,
        unreconciled_receipts: u64::try_from(unreconciled_receipts)
            .map_err(|_| "receipt evidence is invalid")?,
        process_fatal_integrity_events: u64::try_from(fatal_integrity)
            .map_err(|_| "integrity evidence is invalid")?,
        identity_mismatches: 0,
        prediction_error_bps,
        daily_loss_wei: daily_loss,
        daily_loss_limit_wei: daily_limit,
        consecutive_losses: u64::try_from(consecutive_losses)
            .map_err(|_| "loss evidence is invalid")?,
        wallet_gas_reserve_wei: reserve,
        gas_reserve_floor_wei: previous.gas_reserve_floor_wei,
        maximum_quote_age_ms,
        maximum_candidate_age_ms,
    };
    sqlx::query(
        "UPDATE live_canary.economic_control
         SET gas_reserve_wei = $1::numeric, updated_at = now()
         WHERE singleton",
    )
    .bind(reserve.to_string())
    .execute(&mut *transaction)
    .await
    .map_err(|_| "gas reserve update failed")?;

    if evidence.fork_attempts > 0
        && evidence.fork_passes.saturating_mul(10_000)
            < evidence.fork_attempts.saturating_mul(9_500)
    {
        route_failure(&mut transaction, &previous, "fork_pass_rate").await?;
    } else if evidence.reconciled_outcomes > 0 && evidence.prediction_error_bps > 1_000 {
        route_failure(&mut transaction, &previous, "prediction_error").await?;
    } else if evidence.rpc_disagreements >= 2 {
        fail_close_execution_authority(&mut transaction, "rpc_disagreement", Utc::now())
            .await
            .map_err(|_| "provider disagreement fail-close failed")?;
    } else if evidence.wallet_gas_reserve_wei <= evidence.gas_reserve_floor_wei {
        route_failure(&mut transaction, &previous, "gas_reserve_floor").await?;
    } else if let Ok(Transition::Promote { level, .. }) =
        evaluate_promotion(previous.phase, previous.level, &evidence)
    {
        apply_promotion(&mut transaction, &previous, level).await?;
    }
    transaction
        .commit()
        .await
        .map_err(|_| "economic supervision commit failed")?;
    println!(
        "ECONOMIC_SUPERVISION_OK: phase={} level={} outcomes={} net_pnl_wei={}",
        previous.phase.as_str(),
        previous.level.as_str(),
        evidence.reconciled_outcomes,
        evidence.aggregate_realized_net_pnl_wei
    );
    Ok(())
}

async fn wallet_gas_reserve() -> ControlResult<u128> {
    let wallet = CanonicalAddress::parse(&required("WALLET_ADDRESS")?)
        .map_err(|_| "wallet address is invalid")?;
    let url =
        Url::parse(&required("PRODUCTION_RPC_URL")?).map_err(|_| "primary RPC URL is invalid")?;
    let allowlist = required("LIVE_EXECUTOR_RPC_ALLOWLIST")?
        .split(',')
        .map(|value| Url::parse(value).map_err(|_| "RPC allowlist is invalid"))
        .collect::<Result<Vec<_>, _>>()?;
    HttpExecutionRpc::new_production_authenticated(
        url,
        &allowlist,
        &required("LIVE_EXECUTOR_RPC_HEADER_NAME")?,
        &required("LIVE_EXECUTOR_RPC_HEADER_FILE")?,
    )
    .map_err(|_| "primary RPC is not allowlisted")?
    .wallet_balance(wallet)
    .await
    .map_err(|_| "wallet gas reserve is unavailable".into())
}

fn nonnegative_milliseconds(duration: chrono::Duration) -> ControlResult<u64> {
    u64::try_from(duration.num_milliseconds().max(0))
        .map_err(|_| "latency evidence is invalid".into())
}

async fn route_failure(
    transaction: &mut Transaction<'_, Postgres>,
    previous: &EconomicState,
    reason: &'static str,
) -> ControlResult<()> {
    sqlx::query(
        "UPDATE live_canary.autonomous_route_controls
         SET enabled = false, kill_switch = true, disarm_reason = $2,
             cooldown_until = NULL, control_hash = NULL, control_contract = NULL,
             control_epoch = control_epoch + 1, updated_at = now()
         WHERE route_fingerprint = $1",
    )
    .bind(
        previous
            .route_fingerprint
            .as_deref()
            .ok_or("economic route is unavailable")?,
    )
    .bind(reason)
    .execute(&mut **transaction)
    .await
    .map_err(|_| "route disarm failed")?;
    let next_epoch = previous.control_epoch + 1;
    sqlx::query(
        "UPDATE live_canary.economic_control
         SET phase = 'DISARMED_FAILURE', cooldown_until = NULL,
             control_epoch = $1, last_transition_reason = $2,
             updated_at = now()
         WHERE singleton",
    )
    .bind(next_epoch)
    .bind(reason)
    .execute(&mut **transaction)
    .await
    .map_err(|_| "economic route disarm failed")?;
    insert_transition(
        transaction,
        previous,
        EconomicPhase::DisarmedFailure,
        previous.level,
        reason,
        None,
        previous.release_sha.as_deref(),
        next_epoch,
    )
    .await
}

async fn apply_promotion(
    transaction: &mut Transaction<'_, Postgres>,
    previous: &EconomicState,
    next_level: SizeLevel,
) -> ControlResult<()> {
    let global = sqlx::query(
        "SELECT daily_loss_limit::text AS daily_loss_limit, control_epoch
         FROM live_canary.autonomous_global_control
         WHERE singleton FOR UPDATE",
    )
    .fetch_one(&mut **transaction)
    .await
    .map_err(|_| "global promotion control is unavailable")?;
    let route = sqlx::query(
        "SELECT route_policy_hash, control_epoch
         FROM live_canary.autonomous_route_controls
         WHERE route_fingerprint = $1 FOR UPDATE",
    )
    .bind(
        previous
            .route_fingerprint
            .as_deref()
            .ok_or("economic route is unavailable")?,
    )
    .fetch_one(&mut **transaction)
    .await
    .map_err(|_| "route promotion control is unavailable")?;
    let revenue_lane_rows = sqlx::query(
        "SELECT lane, armed, kill_switch,
                maximum_input_amount::text AS maximum_input_amount
         FROM live_canary.revenue_lane_controls
         WHERE lane IN ('atlas_solver', 'aave_liquidation')
         ORDER BY lane
         FOR UPDATE",
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| "revenue lane promotion control is unavailable")?;
    let revenue_lane_states = revenue_lane_rows
        .iter()
        .map(|row| {
            Ok((
                row.try_get::<String, _>("lane")
                    .map_err(|_| "revenue lane promotion control is invalid")?,
                row.try_get::<bool, _>("armed")
                    .map_err(|_| "revenue lane promotion control is invalid")?,
                row.try_get::<bool, _>("kill_switch")
                    .map_err(|_| "revenue lane promotion control is invalid")?,
                row_u128_text(row, "maximum_input_amount")?,
            ))
        })
        .collect::<ControlResult<Vec<_>>>()?;
    let active_revenue_lanes =
        validate_revenue_lane_size_authority(&revenue_lane_states, previous.level.amount_wei())?;
    let global_epoch = global
        .try_get::<i64, _>("control_epoch")
        .map_err(|_| "global promotion control is invalid")?
        + 1;
    let route_epoch = route
        .try_get::<i64, _>("control_epoch")
        .map_err(|_| "route promotion control is invalid")?
        + 1;
    let updated_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let daily_loss_limit: String = global
        .try_get("daily_loss_limit")
        .map_err(|_| "global promotion control is invalid")?;
    let route_policy_hash: String = route
        .try_get("route_policy_hash")
        .map_err(|_| "route promotion control is invalid")?;
    let route_fingerprint = previous
        .route_fingerprint
        .as_deref()
        .ok_or("economic route is unavailable")?;
    let mut global_contract = json!({
        "schema_version": "phoenix.autonomous-global-control.v1",
        "chain_id": 42161,
        "armed": true,
        "kill_switch": false,
        "execution_mode": "live",
        "maximum_input_amount": next_level.amount_wei().to_string(),
        "daily_loss_limit": daily_loss_limit,
        "daily_ordering_budget": "0",
        "maximum_concurrent_candidates": 1,
        "control_epoch": global_epoch,
        "updated_at": updated_at,
        "disarm_reason": Value::Null,
        "control_hash": "0".repeat(64)
    });
    set_hash(
        &mut global_contract,
        "control_hash",
        "global-control",
        "phoenix.autonomous-global-control.v1",
    )?;
    let mut route_contract = json!({
        "schema_version": "phoenix.autonomous-route-control.v1",
        "chain_id": 42161,
        "route_fingerprint": route_fingerprint,
        "route_policy_hash": route_policy_hash,
        "enabled": true,
        "kill_switch": false,
        "current_size_level": next_level.as_str(),
        "maximum_permitted_size": next_level.amount_wei().to_string(),
        "daily_loss_limit": value_text(
            &reviewed_policy_value(route_fingerprint)?,
            "per_route_daily_loss"
        )?,
        "maximum_consecutive_losses": 3,
        "submission_unknown_disarms": true,
        "integrity_failure_disarms": true,
        "cooldown_until": Value::Null,
        "control_epoch": route_epoch,
        "updated_at": updated_at,
        "disarm_reason": Value::Null,
        "control_hash": "0".repeat(64)
    });
    set_hash(
        &mut route_contract,
        "control_hash",
        "route-control",
        "phoenix.autonomous-route-control.v1",
    )?;
    sqlx::query(
        "UPDATE live_canary.autonomous_global_control
         SET maximum_input_amount = $1::numeric, control_epoch = $2,
             control_hash = $3, control_contract = $4, updated_at = $5::timestamptz
         WHERE singleton AND armed AND NOT kill_switch AND execution_mode = 'live'",
    )
    .bind(next_level.amount_wei().to_string())
    .bind(global_epoch)
    .bind(value_text(&global_contract, "control_hash")?)
    .bind(sqlx::types::Json(&global_contract))
    .bind(&updated_at)
    .execute(&mut **transaction)
    .await
    .map_err(|_| "global promotion failed")?;
    sqlx::query(
        "UPDATE live_canary.autonomous_route_controls
         SET current_size_level = $2, maximum_permitted_size = $3::numeric,
             control_epoch = $4, control_hash = $5, control_contract = $6,
             updated_at = $7::timestamptz
         WHERE route_fingerprint = $1 AND enabled AND NOT kill_switch",
    )
    .bind(route_fingerprint)
    .bind(next_level.as_str())
    .bind(next_level.amount_wei().to_string())
    .bind(route_epoch)
    .bind(value_text(&route_contract, "control_hash")?)
    .bind(sqlx::types::Json(&route_contract))
    .bind(&updated_at)
    .execute(&mut **transaction)
    .await
    .map_err(|_| "route promotion failed")?;
    let revenue_lanes = sqlx::query(
        "UPDATE live_canary.revenue_lane_controls
         SET maximum_input_amount = $1::numeric,
             control_epoch = control_epoch + 1,
             updated_at = $2::timestamptz
         WHERE lane IN ('atlas_solver', 'aave_liquidation')
           AND armed AND NOT kill_switch",
    )
    .bind(next_level.amount_wei().to_string())
    .bind(&updated_at)
    .execute(&mut **transaction)
    .await
    .map_err(|_| "revenue lane promotion failed")?;
    if revenue_lanes.rows_affected() != active_revenue_lanes {
        return Err("revenue lane size authorities diverged during promotion".into());
    }
    let next_epoch = previous.control_epoch + 1;
    sqlx::query(
        "UPDATE live_canary.economic_control
         SET phase = $1, current_size_level = $2,
             current_input_wei = $3::numeric, control_epoch = $4,
             last_transition_reason = 'promotion_gate_passed',
             updated_at = $5::timestamptz
         WHERE singleton",
    )
    .bind(next_level.phase().as_str())
    .bind(next_level.as_str())
    .bind(next_level.amount_wei().to_string())
    .bind(next_epoch)
    .bind(&updated_at)
    .execute(&mut **transaction)
    .await
    .map_err(|_| "economic promotion failed")?;
    insert_transition(
        transaction,
        previous,
        next_level.phase(),
        next_level,
        "promotion_gate_passed",
        None,
        previous.release_sha.as_deref(),
        next_epoch,
    )
    .await
}

fn validate_revenue_lane_size_authority(
    lanes: &[(String, bool, bool, u128)],
    expected_input: u128,
) -> ControlResult<u64> {
    if lanes.len() != 2 || lanes[0].0 != "aave_liquidation" || lanes[1].0 != "atlas_solver" {
        return Err("the exact revenue lane set is unavailable".into());
    }
    let active = lanes
        .iter()
        .filter(|(_, armed, kill, _)| *armed && !*kill)
        .count();
    let closed = lanes
        .iter()
        .filter(|(_, armed, kill, _)| !*armed && *kill)
        .count();
    if active == 2 {
        if lanes
            .iter()
            .any(|(_, _, _, maximum)| *maximum != expected_input)
        {
            return Err("revenue lane size authority diverged from economic control".into());
        }
        Ok(2)
    } else if closed == 2 {
        Ok(0)
    } else {
        Err("revenue lane activation states diverged".into())
    }
}

async fn disarm(pool: &PgPool) -> ControlResult<()> {
    if required("PHOENIX_AUTONOMOUS_DISARM_ACK")? != DISARM_ACK {
        return Err("disarm acknowledgement is invalid".into());
    }
    require_schema(pool).await?;
    let reason = env::var("PHOENIX_AUTONOMOUS_DISARM_REASON")
        .ok()
        .filter(|value| !value.is_empty() && value.len() <= 128)
        .unwrap_or_else(|| "operator_rollback".to_string());
    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| "database transaction failed")?;
    let previous = economic_state_for_update(&mut transaction).await?;
    sqlx::query(
        "UPDATE live_canary.control
         SET armed = false, kill_switch = true, disarm_reason = $1, updated_at = now()
         WHERE singleton",
    )
    .bind(&reason)
    .execute(&mut *transaction)
    .await
    .map_err(|_| "legacy execution control disarm failed")?;
    sqlx::query(
        "UPDATE live_canary.autonomous_global_control
         SET armed = false, kill_switch = true, execution_mode = 'disarmed',
             disarm_reason = $1, control_hash = NULL, control_contract = NULL,
             updated_at = now()
         WHERE singleton",
    )
    .bind(&reason)
    .execute(&mut *transaction)
    .await
    .map_err(|_| "global autonomous control disarm failed")?;
    sqlx::query(
        "UPDATE live_canary.autonomous_route_controls
         SET enabled = false, kill_switch = true, disarm_reason = $1,
             cooldown_until = NULL, control_hash = NULL, control_contract = NULL,
             control_epoch = control_epoch + 1, updated_at = now()",
    )
    .bind(&reason)
    .execute(&mut *transaction)
    .await
    .map_err(|_| "route autonomous control disarm failed")?;
    sqlx::query(
        "UPDATE live_canary.revenue_lane_controls
         SET armed = false, kill_switch = true, disarm_reason = $1,
             control_epoch = control_epoch + 1, updated_at = now()
         WHERE lane IN ('atlas_solver', 'aave_liquidation')",
    )
    .bind(&reason)
    .execute(&mut *transaction)
    .await
    .map_err(|_| "revenue lane control disarm failed")?;
    sqlx::query(
        "UPDATE live_canary.autonomous_candidates
         SET status = 'disarmed', updated_at = now()
         WHERE status IN (
             'materialized', 'approval_pending', 'approved',
             'request_materialized', 'claimed', 'signed'
         )",
    )
    .execute(&mut *transaction)
    .await
    .map_err(|_| "candidate disarm failed")?;
    let next_epoch = previous.control_epoch + 1;
    sqlx::query(
        "UPDATE live_canary.economic_control
         SET phase = 'DISARMED_FAILURE', cooldown_until = NULL,
             control_epoch = $1, last_transition_reason = $2,
             updated_at = now()
         WHERE singleton",
    )
    .bind(next_epoch)
    .bind(&reason)
    .execute(&mut *transaction)
    .await
    .map_err(|_| "economic control disarm failed")?;
    insert_transition(
        &mut transaction,
        &previous,
        EconomicPhase::DisarmedFailure,
        previous.level,
        &reason,
        None,
        previous.release_sha.as_deref(),
        next_epoch,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|_| "disarm commit failed")?;
    println!("AUTONOMOUS_DISARM_OK: reason={reason}");
    Ok(())
}

#[derive(Clone, Debug)]
struct MaxReviewedAuthorityFacts {
    current_input_wei: u128,
    maximum_reviewed_input_wei: u128,
    revenue_lanes: Vec<(String, bool, bool, u128)>,
    generic_controls_closed: bool,
    submission_lock_free: bool,
    active_attempts: i64,
    unresolved_submissions: i64,
    active_atlas_requests: i64,
    current_daily_loss: u128,
    daily_loss_limit: u128,
}

fn validate_max_reviewed_authority_transition(
    previous: &EconomicState,
    release_sha: &str,
    facts: &MaxReviewedAuthorityFacts,
) -> ControlResult<()> {
    if !canonical_hex(release_sha, 40) || previous.release_sha.as_deref() != Some(release_sha) {
        return Err("MAX_REVIEWED authority does not bind the current release".into());
    }
    if previous.phase != EconomicPhase::DisarmedEvidence {
        return Err("MAX_REVIEWED authority requires DISARMED_EVIDENCE".into());
    }
    if facts.maximum_reviewed_input_wei != MAXIMUM_REVIEWED_INPUT_WEI
        || facts.current_input_wei != previous.level.amount_wei()
    {
        return Err("economic size authority is invalid".into());
    }
    if validate_revenue_lane_size_authority(&facts.revenue_lanes, previous.level.amount_wei())? != 0
    {
        return Err("MAX_REVIEWED authority requires both revenue lanes disarmed".into());
    }
    if !facts.generic_controls_closed {
        return Err("MAX_REVIEWED authority requires generic execution controls disarmed".into());
    }
    if !facts.submission_lock_free {
        return Err("MAX_REVIEWED authority is blocked by the global revenue lock".into());
    }
    if facts.active_attempts != 0 {
        return Err("MAX_REVIEWED authority is blocked by an active execution attempt".into());
    }
    if facts.unresolved_submissions != 0 {
        return Err("MAX_REVIEWED authority is blocked by an unresolved submission".into());
    }
    if facts.active_atlas_requests != 0 {
        return Err("MAX_REVIEWED authority is blocked by an active Atlas request".into());
    }
    if facts.current_daily_loss >= facts.daily_loss_limit {
        return Err("MAX_REVIEWED authority is blocked by the daily-loss limit".into());
    }
    Ok(())
}

fn validate_hunter_readiness(payload: &Value, now_millis: i64) -> ControlResult<()> {
    if payload.get("ok").and_then(Value::as_bool) != Some(true)
        || payload.get("hunting_health").and_then(Value::as_bool) != Some(true)
        || payload
            .get("exact_execution_readiness")
            .and_then(Value::as_bool)
            != Some(true)
        || payload.get("atlas_connected").and_then(Value::as_bool) != Some(true)
        || payload
            .get("provider_recovery_state")
            .and_then(Value::as_str)
            != Some("ready")
        || payload.get("degraded_reason").and_then(Value::as_str) != Some("")
    {
        return Err("exact Aave/Atlas readiness is not green".into());
    }
    let circuit_until = payload
        .get("provider_circuit_open_until_unix_millis")
        .and_then(Value::as_i64)
        .ok_or("provider circuit evidence is invalid")?;
    if circuit_until > now_millis {
        return Err("provider circuit is open".into());
    }
    let primary = payload
        .get("primary_provider_id")
        .and_then(Value::as_str)
        .filter(|value| *value == "production-nownodes-arbitrum")
        .ok_or("single-primary readiness evidence is invalid")?;
    if payload.get("primary").and_then(Value::as_str) != Some(primary)
        || !payload.get("confirmation").is_some_and(Value::is_null)
        || !payload
            .get("confirmation_provider_id")
            .is_some_and(Value::is_null)
        || payload.get("quorum").and_then(Value::as_u64) != Some(1)
        || payload
            .get("last_primary_exact_at")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .is_none()
    {
        return Err("single-primary readiness evidence is invalid".into());
    }
    Ok(())
}

async fn require_max_reviewed_runtime_readiness() -> ControlResult<()> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| "readiness client initialization failed")?;
    let gateway = client
        .get(RPC_GATEWAY_READINESS_URL)
        .send()
        .await
        .map_err(|_| "RPC Gateway readiness is unavailable")?;
    if !gateway.status().is_success()
        || gateway
            .content_length()
            .is_some_and(|length| length > MAX_READINESS_RESPONSE_BYTES)
    {
        return Err("RPC Gateway readiness is not green".into());
    }
    let gateway_body = gateway
        .bytes()
        .await
        .map_err(|_| "RPC Gateway readiness is unavailable")?;
    if gateway_body.len() as u64 > MAX_READINESS_RESPONSE_BYTES
        || std::str::from_utf8(&gateway_body).map(str::trim).ok() != Some("ready")
    {
        return Err("RPC Gateway readiness is invalid".into());
    }

    let hunter = client
        .get(ATLAS_HUNTER_READINESS_URL)
        .send()
        .await
        .map_err(|_| "Aave/Atlas hunter readiness is unavailable")?;
    if !hunter.status().is_success()
        || hunter
            .content_length()
            .is_some_and(|length| length > MAX_READINESS_RESPONSE_BYTES)
    {
        return Err("Aave/Atlas hunter readiness is not green".into());
    }
    let hunter_body = hunter
        .bytes()
        .await
        .map_err(|_| "Aave/Atlas hunter readiness is unavailable")?;
    if hunter_body.len() as u64 > MAX_READINESS_RESPONSE_BYTES {
        return Err("Aave/Atlas hunter readiness is invalid".into());
    }
    let payload: Value = serde_json::from_slice(&hunter_body)
        .map_err(|_| "Aave/Atlas hunter readiness is invalid")?;
    validate_hunter_readiness(&payload, Utc::now().timestamp_millis())
}

async fn set_revenue_size_max_reviewed() -> ControlResult<()> {
    if required("PHOENIX_SET_REVENUE_SIZE_ACK")? != SET_REVENUE_SIZE_MAX_REVIEWED_ACK {
        return Err("MAX_REVIEWED revenue size acknowledgement is invalid".into());
    }
    require_signerless_control()?;
    let release_sha = required("PHOENIX_RELEASE_SHA")?;
    if !canonical_hex(&release_sha, 40) {
        return Err("release SHA is invalid".into());
    }
    if required_u128("LIVE_EXECUTOR_MAX_INPUT_AMOUNT")? != MAXIMUM_REVIEWED_INPUT_WEI {
        return Err("configured maximum input must equal MAXIMUM_REVIEWED_INPUT_WEI".into());
    }
    let daily_loss_limit = required_u128("LIVE_EXECUTOR_MAX_DAILY_LOSS_WEI")?;
    let owner_evidence = configured_preflight_from_environment().await?;
    let owner_state = owner_evidence
        .get("final_state")
        .ok_or("owner configuration evidence is invalid")?;
    if owner_evidence.get("status").and_then(Value::as_str) != Some("ready-paused")
        || owner_evidence.get("release_sha").and_then(Value::as_str) != Some(release_sha.as_str())
        || owner_state
            .get("configuration_complete")
            .and_then(Value::as_bool)
            != Some(true)
        || owner_state.get("paused").and_then(Value::as_bool) != Some(true)
        || owner_state
            .get("maximum_input_amount")
            .and_then(Value::as_str)
            .and_then(|value| value.parse::<u128>().ok())
            != Some(MAXIMUM_REVIEWED_INPUT_WEI)
    {
        return Err("executor is not fully configured and paused at MAX_REVIEWED".into());
    }
    require_max_reviewed_runtime_readiness().await?;

    let pool = database_pool().await?;
    require_schema(&pool).await?;
    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| "database transaction failed")?;
    let previous = economic_state_for_update(&mut transaction).await?;
    let economic = sqlx::query(
        "SELECT current_input_wei::text AS current_input_wei,
                maximum_reviewed_input_wei::text AS maximum_reviewed_input_wei
         FROM live_canary.economic_control WHERE singleton",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "economic size authority is unavailable")?;
    let current_input_wei = row_u128_text(&economic, "current_input_wei")?;
    let maximum_reviewed_input_wei = row_u128_text(&economic, "maximum_reviewed_input_wei")?;

    let revenue_lane_rows = sqlx::query(
        "SELECT lane, armed, kill_switch,
                maximum_input_amount::text AS maximum_input_amount
         FROM live_canary.revenue_lane_controls
         WHERE lane IN ('atlas_solver', 'aave_liquidation')
         ORDER BY lane FOR UPDATE",
    )
    .fetch_all(&mut *transaction)
    .await
    .map_err(|_| "revenue lane controls are unavailable")?;
    let revenue_lanes = revenue_lane_rows
        .iter()
        .map(|row| {
            Ok((
                row.try_get::<String, _>("lane")
                    .map_err(|_| "revenue lane control is invalid")?,
                row.try_get::<bool, _>("armed")
                    .map_err(|_| "revenue lane control is invalid")?,
                row.try_get::<bool, _>("kill_switch")
                    .map_err(|_| "revenue lane control is invalid")?,
                row_u128_text(row, "maximum_input_amount")?,
            ))
        })
        .collect::<ControlResult<Vec<_>>>()?;

    let submission_lock = sqlx::query(
        "SELECT active_lane, active_identity, acquired_at
         FROM live_canary.global_revenue_submission_lock
         WHERE singleton FOR UPDATE",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "global revenue submission lock is unavailable")?;
    let submission_lock_free = submission_lock
        .try_get::<Option<String>, _>("active_lane")
        .map_err(|_| "global revenue submission lock is invalid")?
        .is_none()
        && submission_lock
            .try_get::<Option<String>, _>("active_identity")
            .map_err(|_| "global revenue submission lock is invalid")?
            .is_none()
        && submission_lock
            .try_get::<Option<DateTime<Utc>>, _>("acquired_at")
            .map_err(|_| "global revenue submission lock is invalid")?
            .is_none();
    let runtime = sqlx::query(
        "WITH bounds AS (
           SELECT date_trunc('day', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' AS start_at
         ), direct_loss AS (
           SELECT COALESCE(SUM(CASE WHEN net_pnl_wei < 0 THEN -net_pnl_wei ELSE 0 END), 0) AS amount
           FROM live_canary.execution_outcomes, bounds
           WHERE recorded_at >= bounds.start_at AND recorded_at < bounds.start_at + interval '1 day'
         ), atlas_loss AS (
           SELECT COALESCE(SUM(i.solver_gas_limit::numeric * i.oracle_gas_price_wei), 0) AS amount
           FROM live_canary.atlas_solver_requests r
           JOIN live_canary.atlas_auction_ingress i ON i.auction_id = r.auction_id
           CROSS JOIN bounds
           WHERE r.updated_at >= bounds.start_at AND r.updated_at < bounds.start_at + interval '1 day'
             AND (
               r.status IN ('signed', 'submitted', 'submission_unknown')
               OR (
                 r.status = 'lost'
                 AND (r.submission_response_hash IS NOT NULL OR r.inclusion_transaction_hash IS NOT NULL)
               )
             )
         )
         SELECT NOT legacy.armed AND legacy.kill_switch
                    AND NOT global.armed AND global.kill_switch
                    AND global.execution_mode = 'disarmed'
                    AND EXISTS (SELECT 1 FROM live_canary.autonomous_route_controls)
                    AND NOT EXISTS (
                      SELECT 1 FROM live_canary.autonomous_route_controls
                      WHERE enabled OR NOT kill_switch
                    ) AS generic_controls_closed,
                (SELECT count(*) FROM live_canary.execution_attempts
                 WHERE status IN (
                   'claimed', 'nonce_allocated', 'submission_unknown', 'pending', 'timed_out'
                 )) AS active_attempts,
                (SELECT count(*) FROM live_canary.execution_attempts
                 WHERE status IN ('submission_unknown', 'pending', 'timed_out'))
                   AS unresolved_submissions,
                (SELECT count(*) FROM live_canary.atlas_solver_requests
                 WHERE status IN ('claimed', 'signed', 'submitted', 'submission_unknown'))
                   AS active_atlas_requests,
                (direct_loss.amount + atlas_loss.amount)::text AS current_daily_loss
         FROM live_canary.control legacy
         CROSS JOIN live_canary.autonomous_global_control global
         CROSS JOIN direct_loss
         CROSS JOIN atlas_loss
         WHERE legacy.singleton AND global.singleton",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "MAX_REVIEWED runtime authority is unavailable")?;
    let facts = MaxReviewedAuthorityFacts {
        current_input_wei,
        maximum_reviewed_input_wei,
        revenue_lanes,
        generic_controls_closed: runtime
            .try_get("generic_controls_closed")
            .map_err(|_| "MAX_REVIEWED runtime authority is invalid")?,
        submission_lock_free,
        active_attempts: runtime
            .try_get("active_attempts")
            .map_err(|_| "MAX_REVIEWED runtime authority is invalid")?,
        unresolved_submissions: runtime
            .try_get("unresolved_submissions")
            .map_err(|_| "MAX_REVIEWED runtime authority is invalid")?,
        active_atlas_requests: runtime
            .try_get("active_atlas_requests")
            .map_err(|_| "MAX_REVIEWED runtime authority is invalid")?,
        current_daily_loss: row_u128_text(&runtime, "current_daily_loss")?,
        daily_loss_limit,
    };
    validate_max_reviewed_authority_transition(&previous, &release_sha, &facts)?;

    if previous.level == SizeLevel::MaxReviewed && current_input_wei == MAXIMUM_REVIEWED_INPUT_WEI {
        transaction
            .commit()
            .await
            .map_err(|_| "MAX_REVIEWED authority verification commit failed")?;
        println!(
            "REVENUE_SIZE_MAX_REVIEWED_OK: status=already-set size_level=MAX_REVIEWED maximum_input_amount={}",
            MAXIMUM_REVIEWED_INPUT_WEI
        );
        return Ok(());
    }

    let next_epoch = previous.control_epoch + 1;
    let updated = sqlx::query(
        "UPDATE live_canary.economic_control
         SET current_size_level = 'MAX_REVIEWED',
             current_input_wei = $1::numeric,
             control_epoch = $2,
             last_transition_reason = 'owner_accepted_max_reviewed_hunt',
             updated_at = now()
         WHERE singleton AND phase = 'DISARMED_EVIDENCE'
           AND release_sha = $3 AND control_epoch = $4",
    )
    .bind(MAXIMUM_REVIEWED_INPUT_WEI.to_string())
    .bind(next_epoch)
    .bind(&release_sha)
    .bind(previous.control_epoch)
    .execute(&mut *transaction)
    .await
    .map_err(|_| "MAX_REVIEWED economic authority update failed")?;
    if updated.rows_affected() != 1 {
        return Err("MAX_REVIEWED economic authority changed concurrently".into());
    }
    insert_transition(
        &mut transaction,
        &previous,
        previous.phase,
        SizeLevel::MaxReviewed,
        "owner_accepted_max_reviewed_hunt",
        None,
        Some(&release_sha),
        next_epoch,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|_| "MAX_REVIEWED economic authority commit failed")?;
    println!(
        "REVENUE_SIZE_MAX_REVIEWED_OK: status=updated size_level=MAX_REVIEWED maximum_input_amount={} revenue_lanes_armed=false",
        MAXIMUM_REVIEWED_INPUT_WEI
    );
    Ok(())
}

async fn arm_revenue_lanes() -> ControlResult<()> {
    if required("PHOENIX_REVENUE_LANES_ACK")? != ARM_REVENUE_LANES_ACK {
        return Err("revenue lane acknowledgement is invalid".into());
    }
    require_signerless_control()?;
    let pool = database_pool().await?;
    arm_revenue_lanes_in_pool(&pool).await
}

async fn arm_revenue_lanes_in_pool(pool: &PgPool) -> ControlResult<()> {
    require_schema(pool).await?;

    let reviewed_maximum_input_amount = required_u128("LIVE_EXECUTOR_MAX_INPUT_AMOUNT")?;
    if reviewed_maximum_input_amount != MAXIMUM_REVIEWED_INPUT_WEI {
        return Err(
            "executor maximum input must equal the reviewed maximum before revenue lanes can arm"
                .into(),
        );
    }
    let maximum_gas_limit = required_u128("LIVE_EXECUTOR_MAX_GAS_LIMIT")?;
    let maximum_gas_limit: i64 = maximum_gas_limit
        .try_into()
        .ok()
        .filter(|value| *value > 0)
        .ok_or("maximum gas limit exceeds the database bound")?;
    let maximum_fee_per_gas = required_u128("LIVE_EXECUTOR_MAX_MAX_FEE_PER_GAS_WEI")?;
    let maximum_atlas_bid = required_u128("LIVE_EXECUTOR_MAX_ATLAS_BID_WEI")?;
    let daily_loss_limit = required_u128("LIVE_EXECUTOR_MAX_DAILY_LOSS_WEI")?;
    let retained_profit_floor = required_u128("LIVE_EXECUTOR_MIN_EXPECTED_PROFIT")?;

    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| "database transaction failed")?;
    let economic = sqlx::query(
        "SELECT current_size_level, current_input_wei::text AS current_input_wei
         FROM live_canary.economic_control
         WHERE singleton
         FOR UPDATE",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "economic size authority is unavailable")?;
    let current_size_level: String = economic
        .try_get("current_size_level")
        .map_err(|_| "economic size authority is invalid")?;
    let current_size_level = SizeLevel::try_from(current_size_level.as_str())
        .map_err(|_| "economic size authority is invalid")?;
    let current_input_wei: String = economic
        .try_get("current_input_wei")
        .map_err(|_| "economic size authority is invalid")?;
    let lane_maximum_input_amount = current_input_wei
        .parse::<u128>()
        .ok()
        .filter(|value| *value == current_size_level.amount_wei())
        .ok_or("economic size level and input authority diverged")?;
    let active_lane: Option<String> = sqlx::query_scalar(
        "SELECT active_lane
         FROM live_canary.global_revenue_submission_lock
         WHERE singleton
         FOR UPDATE",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "global revenue submission lock is unavailable")?;
    if active_lane.is_some() {
        return Err("revenue lane activation is blocked by an active submission".into());
    }
    let active_attempts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM live_canary.execution_attempts
         WHERE status IN (
            'claimed', 'nonce_allocated', 'submission_unknown', 'pending', 'timed_out'
         )",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "active-attempt inspection failed")?;
    if active_attempts != 0 {
        return Err("revenue lane activation is blocked by an active attempt".into());
    }
    let active_atlas_requests: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM live_canary.atlas_solver_requests
         WHERE status IN ('claimed', 'signed', 'submitted', 'submission_unknown')",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "active Atlas request inspection failed")?;
    if active_atlas_requests != 0 {
        return Err("revenue lane activation is blocked by an active Atlas request".into());
    }
    let provider_ready: bool = sqlx::query_scalar(
        "SELECT exact_execution_ready
         FROM live_canary.revenue_provider_authority
         WHERE singleton
         FOR UPDATE",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "provider execution authority is unavailable")?;
    if !provider_ready {
        return Err("revenue lane activation requires fresh exact provider authority".into());
    }
    let atlas_daily_charged_exposure: String = sqlx::query_scalar(
        "WITH bounds AS (
           SELECT date_trunc('day', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' AS start_at
         ), direct_loss AS (
           SELECT COALESCE(SUM(CASE WHEN net_pnl_wei < 0 THEN -net_pnl_wei ELSE 0 END), 0) AS amount
           FROM live_canary.execution_outcomes, bounds
           WHERE recorded_at >= bounds.start_at AND recorded_at < bounds.start_at + interval '1 day'
         ), atlas_loss AS (
           SELECT COALESCE(SUM(i.solver_gas_limit::numeric * i.oracle_gas_price_wei), 0) AS amount
           FROM live_canary.atlas_solver_requests r
           JOIN live_canary.atlas_auction_ingress i ON i.auction_id = r.auction_id
           CROSS JOIN bounds
           WHERE r.updated_at >= bounds.start_at AND r.updated_at < bounds.start_at + interval '1 day'
             AND (
               r.status IN ('signed', 'submitted', 'submission_unknown')
               OR (
                 r.status = 'lost'
                 AND (r.submission_response_hash IS NOT NULL OR r.inclusion_transaction_hash IS NOT NULL)
               )
             )
         )
         SELECT (direct_loss.amount + atlas_loss.amount)::text FROM direct_loss, atlas_loss",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "Atlas daily-loss accounting is unavailable")?;
    let atlas_daily_charged_exposure = atlas_daily_charged_exposure
        .parse::<u128>()
        .map_err(|_| "Atlas daily-loss accounting is invalid")?;
    if atlas_daily_charged_exposure >= daily_loss_limit {
        return Err("revenue lane activation is blocked by the Atlas daily-loss limit".into());
    }

    let result = sqlx::query(
        "UPDATE live_canary.revenue_lane_controls
         SET armed = true, kill_switch = false,
             maximum_input_amount = $1::numeric,
             maximum_gas_limit = $2,
             maximum_fee_per_gas = $3::numeric,
             maximum_atlas_bid = $4::numeric,
             daily_loss_limit = $5::numeric,
             retained_profit_floor = $6::numeric,
             disarm_reason = 'owner_accepted_live_hunter',
             control_epoch = control_epoch + 1,
             updated_at = now()
         WHERE lane IN ('atlas_solver', 'aave_liquidation')",
    )
    .bind(lane_maximum_input_amount.to_string())
    .bind(maximum_gas_limit)
    .bind(maximum_fee_per_gas.to_string())
    .bind(maximum_atlas_bid.to_string())
    .bind(daily_loss_limit.to_string())
    .bind(retained_profit_floor.to_string())
    .execute(&mut *transaction)
    .await
    .map_err(|_| "revenue lane activation failed")?;
    if result.rows_affected() != 2 {
        return Err("the exact revenue lane set is unavailable".into());
    }
    transaction
        .commit()
        .await
        .map_err(|_| "revenue lane activation commit failed")?;
    println!(
        "REVENUE_LANES_ARMED_OK: lanes=atlas_solver,aave_liquidation armed=true kill_switch=false maximum_input_amount={} maximum_gas_limit={} maximum_fee_per_gas={} maximum_atlas_bid={} daily_loss_limit={} retained_profit_floor={}",
        lane_maximum_input_amount,
        maximum_gas_limit,
        maximum_fee_per_gas,
        maximum_atlas_bid,
        daily_loss_limit,
        retained_profit_floor
    );
    Ok(())
}

async fn status(pool: &PgPool) -> Result<(), &'static str> {
    require_schema(pool).await?;
    let row = sqlx::query(
        "SELECT armed, kill_switch, execution_mode, control_epoch,
                control_hash IS NOT NULL AS hash_present
         FROM live_canary.autonomous_global_control
         WHERE singleton",
    )
    .fetch_one(pool)
    .await
    .map_err(|_| "global control is unavailable")?;
    let route = sqlx::query(
        "SELECT route_fingerprint, enabled, kill_switch, control_epoch,
                control_hash IS NOT NULL AS hash_present
         FROM live_canary.autonomous_route_controls
         ORDER BY route_fingerprint
         LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .map_err(|_| "route control is unavailable")?;
    let revenue_lanes = sqlx::query(
        "SELECT lane, armed, kill_switch,
                maximum_input_amount::text AS maximum_input_amount,
                maximum_gas_limit,
                maximum_fee_per_gas::text AS maximum_fee_per_gas,
                maximum_atlas_bid::text AS maximum_atlas_bid,
                daily_loss_limit::text AS daily_loss_limit,
                retained_profit_floor::text AS retained_profit_floor,
                disarm_reason, control_epoch, updated_at
         FROM live_canary.revenue_lane_controls
         WHERE lane IN ('atlas_solver', 'aave_liquidation')
         ORDER BY lane",
    )
    .fetch_all(pool)
    .await
    .map_err(|_| "revenue lane controls are unavailable")?;
    if revenue_lanes.len() != 2 {
        return Err("the exact revenue lane set is unavailable");
    }
    let economic = sqlx::query(
        "SELECT phase, current_size_level, current_input_wei::text AS current_input_wei,
                maximum_reviewed_input_wei::text AS maximum_reviewed_input_wei,
                release_sha, readiness_id, authorization_id, cooldown_until,
                gas_reserve_wei::text AS gas_reserve_wei,
                gas_reserve_floor_wei::text AS gas_reserve_floor_wei,
                control_epoch, last_transition_reason, updated_at
         FROM live_canary.economic_control WHERE singleton",
    )
    .fetch_one(pool)
    .await
    .map_err(|_| "economic control is unavailable")?;
    let provider_authority = sqlx::query(
        "SELECT exact_execution_ready, gate_reason, gate_updated_at, request_evidence_not_before,
                recovery_status,
                failure_reason, failure_control_epoch, failure_transition_at,
                failure_release_sha, restore_phase, restore_size_level, sample_count,
                sample_1_at, sample_1_primary_provider, sample_1_confirmation_provider,
                sample_2_at, sample_2_primary_provider, sample_2_confirmation_provider,
                sample_3_at, sample_3_primary_provider, sample_3_confirmation_provider,
                recovery_attempted_total, recovery_succeeded_total, recovery_blocked_total,
                last_block_reason, recovery_evidence_hash, last_recovered_at, updated_at
         FROM live_canary.revenue_provider_authority WHERE singleton",
    )
    .fetch_one(pool)
    .await
    .map_err(|_| "provider execution authority is unavailable")?;
    let provider_sample = |index: usize| -> Value {
        json!({
            "observed_at": provider_authority
                .try_get::<Option<DateTime<Utc>>, _>(format!("sample_{index}_at").as_str())
                .ok()
                .flatten(),
            "primary_provider": provider_authority
                .try_get::<Option<String>, _>(format!("sample_{index}_primary_provider").as_str())
                .ok()
                .flatten(),
            "confirmation": null,
            "quorum": 1,
        })
    };
    let provider_sample_count = provider_authority
        .try_get::<i16, _>("sample_count")
        .map_err(|_| "provider recovery sample count is invalid")?;
    let provider_samples = (1..=provider_sample_count)
        .map(|index| provider_sample(index as usize))
        .collect::<Vec<_>>();
    let payload = json!({
        "schema": "phoenix.autonomous-live-status.v2",
        "chain_id": 42161,
        "global": {
            "armed": row.try_get::<bool, _>("armed").map_err(|_| "global control is invalid")?,
            "kill_switch": row.try_get::<bool, _>("kill_switch").map_err(|_| "global control is invalid")?,
            "execution_mode": row.try_get::<String, _>("execution_mode").map_err(|_| "global control is invalid")?,
            "control_epoch": row.try_get::<i64, _>("control_epoch").map_err(|_| "global control is invalid")?,
            "hash_present": row.try_get::<bool, _>("hash_present").map_err(|_| "global control is invalid")?
        },
        "route": route.map(|route| json!({
            "route_fingerprint": route.try_get::<String, _>("route_fingerprint").ok(),
            "enabled": route.try_get::<bool, _>("enabled").ok(),
            "kill_switch": route.try_get::<bool, _>("kill_switch").ok(),
            "control_epoch": route.try_get::<i64, _>("control_epoch").ok(),
            "hash_present": route.try_get::<bool, _>("hash_present").ok()
        })),
        "revenue_lanes": revenue_lanes.into_iter().map(|lane| json!({
            "lane": lane.try_get::<String, _>("lane").ok(),
            "armed": lane.try_get::<bool, _>("armed").ok(),
            "kill_switch": lane.try_get::<bool, _>("kill_switch").ok(),
            "maximum_input_amount": lane.try_get::<String, _>("maximum_input_amount").ok(),
            "maximum_gas_limit": lane.try_get::<i64, _>("maximum_gas_limit").ok(),
            "maximum_fee_per_gas": lane.try_get::<String, _>("maximum_fee_per_gas").ok(),
            "maximum_atlas_bid": lane.try_get::<String, _>("maximum_atlas_bid").ok(),
            "daily_loss_limit": lane.try_get::<String, _>("daily_loss_limit").ok(),
            "retained_profit_floor": lane.try_get::<String, _>("retained_profit_floor").ok(),
            "disarm_reason": lane.try_get::<String, _>("disarm_reason").ok(),
            "control_epoch": lane.try_get::<i64, _>("control_epoch").ok(),
            "updated_at": lane.try_get::<chrono::DateTime<Utc>, _>("updated_at").ok()
        })).collect::<Vec<_>>(),
        "economic": {
            "phase": economic.try_get::<String, _>("phase").map_err(|_| "economic control is invalid")?,
            "current_size_level": economic.try_get::<String, _>("current_size_level").map_err(|_| "economic control is invalid")?,
            "current_input_wei": economic.try_get::<String, _>("current_input_wei").map_err(|_| "economic control is invalid")?,
            "maximum_reviewed_input_wei": economic.try_get::<String, _>("maximum_reviewed_input_wei").map_err(|_| "economic control is invalid")?,
            "release_sha": economic.try_get::<Option<String>, _>("release_sha").map_err(|_| "economic control is invalid")?,
            "readiness_id": economic.try_get::<Option<Uuid>, _>("readiness_id").map_err(|_| "economic control is invalid")?,
            "authorization_id": economic.try_get::<Option<Uuid>, _>("authorization_id").map_err(|_| "economic control is invalid")?,
            "cooldown_until": economic.try_get::<Option<chrono::DateTime<Utc>>, _>("cooldown_until").map_err(|_| "economic control is invalid")?,
            "gas_reserve_wei": economic.try_get::<String, _>("gas_reserve_wei").map_err(|_| "economic control is invalid")?,
            "gas_reserve_floor_wei": economic.try_get::<String, _>("gas_reserve_floor_wei").map_err(|_| "economic control is invalid")?,
            "control_epoch": economic.try_get::<i64, _>("control_epoch").map_err(|_| "economic control is invalid")?,
            "last_transition_reason": economic.try_get::<String, _>("last_transition_reason").map_err(|_| "economic control is invalid")?,
            "updated_at": economic.try_get::<chrono::DateTime<Utc>, _>("updated_at").map_err(|_| "economic control is invalid")?
        },
        "provider_execution_authority": {
            "exact_execution_ready": provider_authority.try_get::<bool, _>("exact_execution_ready").map_err(|_| "provider execution authority is invalid")?,
            "gate_reason": provider_authority.try_get::<String, _>("gate_reason").map_err(|_| "provider execution authority is invalid")?,
            "gate_updated_at": provider_authority.try_get::<DateTime<Utc>, _>("gate_updated_at").map_err(|_| "provider execution authority is invalid")?,
            "request_evidence_not_before": provider_authority.try_get::<DateTime<Utc>, _>("request_evidence_not_before").map_err(|_| "provider execution authority is invalid")?,
            "recovery_status": provider_authority.try_get::<String, _>("recovery_status").map_err(|_| "provider execution authority is invalid")?,
            "failure_reason": provider_authority.try_get::<Option<String>, _>("failure_reason").map_err(|_| "provider execution authority is invalid")?,
            "failure_control_epoch": provider_authority.try_get::<Option<i64>, _>("failure_control_epoch").map_err(|_| "provider execution authority is invalid")?,
            "failure_transition_at": provider_authority.try_get::<Option<DateTime<Utc>>, _>("failure_transition_at").map_err(|_| "provider execution authority is invalid")?,
            "failure_release_sha": provider_authority.try_get::<Option<String>, _>("failure_release_sha").map_err(|_| "provider execution authority is invalid")?,
            "restore_phase": provider_authority.try_get::<Option<String>, _>("restore_phase").map_err(|_| "provider execution authority is invalid")?,
            "restore_size_level": provider_authority.try_get::<Option<String>, _>("restore_size_level").map_err(|_| "provider execution authority is invalid")?,
            "sample_count": provider_sample_count,
            "samples": provider_samples,
            "recovery_attempted_total": provider_authority.try_get::<i64, _>("recovery_attempted_total").map_err(|_| "provider execution authority is invalid")?,
            "recovery_succeeded_total": provider_authority.try_get::<i64, _>("recovery_succeeded_total").map_err(|_| "provider execution authority is invalid")?,
            "recovery_blocked_total": provider_authority.try_get::<i64, _>("recovery_blocked_total").map_err(|_| "provider execution authority is invalid")?,
            "last_block_reason": provider_authority.try_get::<Option<String>, _>("last_block_reason").map_err(|_| "provider execution authority is invalid")?,
            "recovery_evidence_hash": provider_authority.try_get::<Option<String>, _>("recovery_evidence_hash").map_err(|_| "provider execution authority is invalid")?,
            "last_recovered_at": provider_authority.try_get::<Option<DateTime<Utc>>, _>("last_recovered_at").map_err(|_| "provider execution authority is invalid")?,
            "updated_at": provider_authority.try_get::<DateTime<Utc>, _>("updated_at").map_err(|_| "provider execution authority is invalid")?
        }
    });
    println!(
        "{}",
        serde_json::to_string(&payload).map_err(|_| "status serialization failed")?
    );
    Ok(())
}

async fn reconciliation_status(pool: &PgPool) -> Result<(), &'static str> {
    require_schema(pool).await?;
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*)
         FROM live_canary.execution_attempts
         WHERE status IN (
             'claimed', 'nonce_allocated', 'submission_unknown', 'pending', 'timed_out'
         )",
    )
    .fetch_one(pool)
    .await
    .map_err(|_| "active-attempt inspection failed")?;
    if active != 0 {
        return Err("receipt reconciliation is still active");
    }
    println!("AUTONOMOUS_RECONCILIATION_OK: active_attempts=0");
    Ok(())
}

async fn require_schema(pool: &PgPool) -> Result<(), &'static str> {
    let installed: bool = sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1 FROM live_canary.schema_contract
             WHERE version = 'phoenix.live-canary-schema.v9'
         )",
    )
    .fetch_one(pool)
    .await
    .map_err(|_| "schema inspection failed")?;
    if !installed {
        return Err("phoenix.live-canary-schema.v9 is not installed");
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct EconomicState {
    phase: EconomicPhase,
    level: SizeLevel,
    route_fingerprint: Option<String>,
    release_sha: Option<String>,
    engine_image_digest: Option<String>,
    route_universe_hash: Option<String>,
    route_policy_hash: Option<String>,
    risk_policy_hash: Option<String>,
    executor_code_hash: Option<String>,
    readiness_id: Option<Uuid>,
    authorization_id: Option<Uuid>,
    gas_reserve_floor_wei: u128,
    control_epoch: i64,
    updated_at: DateTime<Utc>,
}

#[allow(clippy::too_many_arguments)]
fn validate_evidence_start(
    previous: &EconomicState,
    release_sha: &str,
    engine_image_digest: &str,
    route_fingerprint: &str,
    route_universe_hash: &str,
    route_policy_hash: &str,
    authority: &EvidenceAuthorityState,
) -> ControlResult<()> {
    if previous.phase != EconomicPhase::DisarmedDeploy {
        return Err("economic control is not in DISARMED_DEPLOY".into());
    }
    if previous.release_sha.as_deref() != Some(release_sha)
        || previous.engine_image_digest.as_deref() != Some(engine_image_digest)
        || previous.route_fingerprint.as_deref() != Some(route_fingerprint)
        || previous.route_universe_hash.as_deref() != Some(route_universe_hash)
        || previous.route_policy_hash.as_deref() != Some(route_policy_hash)
        || previous.risk_policy_hash.as_deref() != Some(route_policy_hash)
    {
        return Err("evidence-start does not bind the current release".into());
    }
    if authority.legacy_armed
        || !authority.legacy_kill_switch
        || authority.global_armed
        || !authority.global_kill_switch
        || !authority.global_disarmed
        || authority.route_enabled
        || !authority.route_kill_switch
    {
        return Err("evidence-start requires fail-closed global and route controls".into());
    }
    if authority.active_attempts != 0 {
        return Err("evidence-start is blocked by an active execution attempt".into());
    }
    if authority.unresolved_receipts != 0 {
        return Err("evidence-start is blocked by unresolved receipt reconciliation".into());
    }
    Ok(())
}

fn validate_readiness_against_evidence(
    previous: &EconomicState,
    binding: &ReadinessBinding,
) -> ControlResult<()> {
    if previous.phase != EconomicPhase::DisarmedEvidence {
        return Err("readiness requires the durable DISARMED_EVIDENCE phase".into());
    }
    if previous.release_sha.as_deref() != Some(binding.release_sha.as_str())
        || previous.engine_image_digest.as_deref() != Some(binding.engine_image_digest.as_str())
        || previous.route_universe_hash.as_deref() != Some(binding.route_universe_hash.as_str())
        || previous.executor_code_hash.as_deref() != Some(binding.executor_code_hash.as_str())
        || binding.risk_policy_hash != binding.route_policy_hash
    {
        return Err("readiness does not bind the current disarmed release".into());
    }
    let policy = reviewed_policy_value(&binding.route_fingerprint)?;
    if value_text(&policy, "policy_hash")? != binding.route_policy_hash
        || value_text(&policy, "route_universe_hash")? != binding.route_universe_hash
    {
        return Err("readiness route is outside the reviewed release universe".into());
    }
    if u64::try_from(previous.control_epoch).ok() != Some(binding.economic_control_epoch) {
        return Err("readiness does not bind the current economic control epoch".into());
    }
    if binding.observed_from < previous.updated_at {
        return Err("readiness observation predates DISARMED_EVIDENCE".into());
    }
    Ok(())
}

async fn economic_state(pool: &PgPool) -> ControlResult<EconomicState> {
    let row = sqlx::query(
        "SELECT phase, current_size_level, route_fingerprint, release_sha,
                engine_image_digest, route_universe_hash, route_policy_hash,
                risk_policy_hash, executor_code_hash,
                readiness_id, authorization_id,
                gas_reserve_floor_wei::text AS gas_reserve_floor_wei,
                control_epoch, updated_at
         FROM live_canary.economic_control
         WHERE singleton",
    )
    .fetch_one(pool)
    .await
    .map_err(|_| "economic control is unavailable")?;
    economic_state_from_row(&row)
}

async fn economic_state_for_update(
    transaction: &mut Transaction<'_, Postgres>,
) -> ControlResult<EconomicState> {
    let row = sqlx::query(
        "SELECT phase, current_size_level, route_fingerprint, release_sha,
                engine_image_digest, route_universe_hash, route_policy_hash,
                risk_policy_hash, executor_code_hash,
                readiness_id, authorization_id,
                gas_reserve_floor_wei::text AS gas_reserve_floor_wei,
                control_epoch, updated_at
         FROM live_canary.economic_control
         WHERE singleton
         FOR UPDATE",
    )
    .fetch_one(&mut **transaction)
    .await
    .map_err(|_| "economic control is unavailable")?;
    economic_state_from_row(&row)
}

fn economic_state_from_row(row: &sqlx::postgres::PgRow) -> ControlResult<EconomicState> {
    let phase = parse_phase(
        &row.try_get::<String, _>("phase")
            .map_err(|_| "economic control is invalid")?,
    )?;
    let level_text: String = row
        .try_get("current_size_level")
        .map_err(|_| "economic control is invalid")?;
    let level =
        SizeLevel::try_from(level_text.as_str()).map_err(|_| "economic size level is invalid")?;
    Ok(EconomicState {
        phase,
        level,
        route_fingerprint: row
            .try_get("route_fingerprint")
            .map_err(|_| "economic control is invalid")?,
        release_sha: row
            .try_get("release_sha")
            .map_err(|_| "economic control is invalid")?,
        engine_image_digest: row
            .try_get("engine_image_digest")
            .map_err(|_| "economic control is invalid")?,
        route_universe_hash: row
            .try_get("route_universe_hash")
            .map_err(|_| "economic control is invalid")?,
        route_policy_hash: row
            .try_get("route_policy_hash")
            .map_err(|_| "economic control is invalid")?,
        risk_policy_hash: row
            .try_get("risk_policy_hash")
            .map_err(|_| "economic control is invalid")?,
        executor_code_hash: row
            .try_get("executor_code_hash")
            .map_err(|_| "economic control is invalid")?,
        readiness_id: row
            .try_get("readiness_id")
            .map_err(|_| "economic control is invalid")?,
        authorization_id: row
            .try_get("authorization_id")
            .map_err(|_| "economic control is invalid")?,
        gas_reserve_floor_wei: row
            .try_get::<String, _>("gas_reserve_floor_wei")
            .map_err(|_| "economic control is invalid")?
            .parse()
            .map_err(|_| "economic gas reserve floor is invalid")?,
        control_epoch: row
            .try_get("control_epoch")
            .map_err(|_| "economic control is invalid")?,
        updated_at: row
            .try_get("updated_at")
            .map_err(|_| "economic control is invalid")?,
    })
}

#[allow(clippy::too_many_arguments)]
async fn insert_transition(
    transaction: &mut Transaction<'_, Postgres>,
    previous: &EconomicState,
    next_phase: EconomicPhase,
    next_level: SizeLevel,
    reason: &str,
    evidence_hash: Option<&str>,
    release_sha: Option<&str>,
    control_epoch: i64,
) -> ControlResult<()> {
    insert_transition_at(
        transaction,
        previous,
        next_phase,
        next_level,
        reason,
        evidence_hash,
        release_sha,
        control_epoch,
        Utc::now(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn insert_transition_at(
    transaction: &mut Transaction<'_, Postgres>,
    previous: &EconomicState,
    next_phase: EconomicPhase,
    next_level: SizeLevel,
    reason: &str,
    evidence_hash: Option<&str>,
    release_sha: Option<&str>,
    control_epoch: i64,
    transitioned_at: DateTime<Utc>,
) -> ControlResult<()> {
    if reason.is_empty()
        || reason.len() > 128
        || evidence_hash.is_some_and(|value| !canonical_hex(value, 64))
    {
        return Err("economic transition evidence is invalid".into());
    }
    let transition_id = Uuid::new_v4();
    let transitioned_at = transitioned_at.to_rfc3339_opts(SecondsFormat::Micros, true);
    let mut contract = json!({
        "schema_version": "phoenix.economic-transition.v1",
        "transition_id": transition_id,
        "from_phase": previous.phase.as_str(),
        "to_phase": next_phase.as_str(),
        "from_size_level": previous.level.as_str(),
        "to_size_level": next_level.as_str(),
        "reason": reason,
        "evidence_hash": evidence_hash,
        "release_sha": release_sha,
        "control_epoch": control_epoch,
        "transitioned_at": transitioned_at,
        "transition_hash": "0".repeat(64)
    });
    set_hash(
        &mut contract,
        "transition_hash",
        "economic-transition",
        "phoenix.economic-transition.v1",
    )?;
    sqlx::query(
        "INSERT INTO live_canary.economic_transitions(
            transition_id, schema_version, from_phase, to_phase,
            from_size_level, to_size_level, reason, evidence_hash,
            release_sha, control_epoch, transition_hash, transitioned_at
         ) VALUES (
            $1, 'phoenix.economic-transition.v1', $2, $3, $4, $5, $6,
            $7, $8, $9, $10, $11::timestamptz
         )",
    )
    .bind(transition_id)
    .bind(previous.phase.as_str())
    .bind(next_phase.as_str())
    .bind(previous.level.as_str())
    .bind(next_level.as_str())
    .bind(reason)
    .bind(evidence_hash)
    .bind(release_sha)
    .bind(control_epoch)
    .bind(value_text(&contract, "transition_hash")?)
    .bind(&transitioned_at)
    .execute(&mut **transaction)
    .await
    .map_err(|_| "economic transition persistence failed")?;
    Ok(())
}

fn parse_phase(value: &str) -> ControlResult<EconomicPhase> {
    match value {
        "DISARMED_DEPLOY" => Ok(EconomicPhase::DisarmedDeploy),
        "DISARMED_EVIDENCE" => Ok(EconomicPhase::DisarmedEvidence),
        "CANARY_READY" => Ok(EconomicPhase::CanaryReady),
        "LIVE_CANARY_MIN" => Ok(EconomicPhase::LiveCanaryMin),
        "LIVE_SCALE_L1" => Ok(EconomicPhase::LiveScaleL1),
        "LIVE_SCALE_L2" => Ok(EconomicPhase::LiveScaleL2),
        "LIVE_SCALE_L3" => Ok(EconomicPhase::LiveScaleL3),
        "LIVE_SCALE_L4" => Ok(EconomicPhase::LiveScaleL4),
        "LIVE_SCALE_L5" => Ok(EconomicPhase::LiveScaleL5),
        "LIVE_MAX_REVIEWED" => Ok(EconomicPhase::LiveMaxReviewed),
        "COOLDOWN" => Ok(EconomicPhase::Cooldown),
        "DISARMED_FAILURE" => Ok(EconomicPhase::DisarmedFailure),
        _ => Err("economic phase is invalid".into()),
    }
}

fn read_control_contract(
    environment_name: &'static str,
    error: &'static str,
) -> ControlResult<(Value, PathBuf)> {
    let path = PathBuf::from(required(environment_name)?);
    let metadata = fs::symlink_metadata(&path).map_err(|_| error)?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > MAX_CONTROL_FILE_BYTES
    {
        return Err(error.into());
    }
    let raw = fs::read(&path).map_err(|_| error)?;
    let value = serde_json::from_slice(&raw).map_err(|_| error)?;
    Ok((value, path))
}

fn image_digest(value: &str) -> ControlResult<String> {
    let digest = value
        .rsplit_once('@')
        .map(|(_, digest)| digest)
        .unwrap_or(value);
    let Some(hex_digest) = digest.strip_prefix("sha256:") else {
        return Err("Engine image digest is invalid".into());
    };
    if !canonical_hex(hex_digest, 64) {
        return Err("Engine image digest is invalid".into());
    }
    Ok(digest.to_string())
}

fn canonical_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn require_signerless_control() -> ControlResult<()> {
    for name in [
        "SIGNER_PRIVATE_KEY",
        "SIGNER_PRIVATE_KEY_FILE",
        "LIVE_EXECUTOR_SIGNER_FILE",
    ] {
        if env::var_os(name).is_some() {
            return Err("evidence-start requires a signerless control process".into());
        }
    }
    Ok(())
}

fn required(name: &'static str) -> ControlResult<String> {
    required_environment_with(name, &mut |name| env::var(name).ok()).map_err(Into::into)
}

fn required_u128(name: &'static str) -> ControlResult<u128> {
    required(name)?
        .parse()
        .ok()
        .filter(|value| *value > 0)
        .ok_or(ControlError::Message(
            "required numeric environment is invalid",
        ))
}

fn value_text<'a>(value: &'a Value, field: &str) -> Result<&'a str, &'static str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or("canonical contract field is invalid")
}

fn value_u128(value: &Value, field: &str) -> Result<u128, &'static str> {
    value_text(value, field)?
        .parse()
        .map_err(|_| "canonical numeric field is invalid")
}

fn verify_hash(value: &Value, field: &str, domain: &str, schema: &str) -> Result<(), &'static str> {
    if value_text(value, field)? != contract_hash(value, field, domain, schema)? {
        return Err("canonical contract hash mismatch");
    }
    Ok(())
}

fn set_hash(
    value: &mut Value,
    field: &str,
    domain: &str,
    schema: &str,
) -> Result<(), &'static str> {
    let digest = contract_hash(value, field, domain, schema)?;
    value
        .as_object_mut()
        .ok_or("canonical contract is invalid")?
        .insert(field.to_string(), Value::String(digest));
    Ok(())
}

fn contract_hash(
    value: &Value,
    field: &str,
    domain: &str,
    schema: &str,
) -> Result<String, &'static str> {
    let mut body = value.clone();
    body.as_object_mut()
        .ok_or("canonical contract is invalid")?
        .remove(field)
        .ok_or("canonical hash field is missing")?;
    let prefix = format!("phoenix.canonical-json.v1:{domain}:{schema}\n");
    Ok(hex::encode(Sha256::digest(
        [prefix.as_bytes(), canonical_json(&body)?.as_slice()].concat(),
    )))
}

fn canonical_json(value: &Value) -> Result<Vec<u8>, &'static str> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) | Value::Number(_) => {
            serde_json::to_vec(value).map_err(|_| "canonical serialization failed")
        }
        Value::Array(values) => {
            let mut output = vec![b'['];
            for (index, child) in values.iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                output.extend(canonical_json(child)?);
            }
            output.push(b']');
            Ok(output)
        }
        Value::Object(values) => {
            let sorted = values.iter().collect::<BTreeMap<_, _>>();
            let mut output = vec![b'{'];
            for (index, (key, child)) in sorted.into_iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                output
                    .extend(serde_json::to_vec(key).map_err(|_| "canonical serialization failed")?);
                output.push(b':');
                output.extend(canonical_json(child)?);
            }
            output.push(b'}');
            Ok(output)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration as ChronoDuration, TimeZone};

    fn evidence_time() -> DateTime<Utc> {
        Utc.timestamp_opt(1_720_000_000, 0)
            .single()
            .expect("valid timestamp")
    }

    fn state(phase: EconomicPhase) -> EconomicState {
        EconomicState {
            phase,
            level: SizeLevel::Min,
            route_fingerprint: Some("route".to_string()),
            release_sha: Some("a".repeat(40)),
            engine_image_digest: Some(format!("sha256:{}", "b".repeat(64))),
            route_universe_hash: Some("c".repeat(64)),
            route_policy_hash: Some("d".repeat(64)),
            risk_policy_hash: Some("d".repeat(64)),
            executor_code_hash: Some("e".repeat(64)),
            readiness_id: None,
            authorization_id: None,
            gas_reserve_floor_wei: 1,
            control_epoch: 7,
            updated_at: evidence_time(),
        }
    }

    fn closed_authority() -> EvidenceAuthorityState {
        EvidenceAuthorityState {
            legacy_armed: false,
            legacy_kill_switch: true,
            global_armed: false,
            global_kill_switch: true,
            global_disarmed: true,
            route_enabled: false,
            route_kill_switch: true,
            active_attempts: 0,
            unresolved_receipts: 0,
        }
    }

    fn max_reviewed_authority_facts() -> MaxReviewedAuthorityFacts {
        MaxReviewedAuthorityFacts {
            current_input_wei: SizeLevel::Min.amount_wei(),
            maximum_reviewed_input_wei: MAXIMUM_REVIEWED_INPUT_WEI,
            revenue_lanes: vec![
                (
                    "aave_liquidation".to_string(),
                    false,
                    true,
                    SizeLevel::Min.amount_wei(),
                ),
                (
                    "atlas_solver".to_string(),
                    false,
                    true,
                    SizeLevel::Min.amount_wei(),
                ),
            ],
            generic_controls_closed: true,
            submission_lock_free: true,
            active_attempts: 0,
            unresolved_submissions: 0,
            active_atlas_requests: 0,
            current_daily_loss: 0,
            daily_loss_limit: 1,
        }
    }

    fn hunter_readiness() -> Value {
        json!({
            "ok": true,
            "hunting_health": true,
            "exact_execution_readiness": true,
            "atlas_connected": true,
            "provider_recovery_state": "ready",
            "provider_current_class_failure_streak": 0,
            "degraded_reason": "",
            "provider_circuit_open_until_unix_millis": 0,
            "primary": "production-nownodes-arbitrum",
            "confirmation": null,
            "quorum": 1,
            "primary_provider_id": "production-nownodes-arbitrum",
            "confirmation_provider_id": null,
            "last_primary_exact_at": "2026-08-10T00:00:00Z"
        })
    }

    fn readiness() -> ReadinessBinding {
        let start = evidence_time();
        ReadinessBinding {
            release_sha: "a".repeat(40),
            engine_image_digest: format!("sha256:{}", "b".repeat(64)),
            route_fingerprint: CURRENT_ROUTE_FINGERPRINT.to_string(),
            route_universe_hash: "84adac686635535486e06e44fcaf90c812dc27273affc5bffc4eebd6c164928c"
                .to_string(),
            route_policy_hash: "d7aff21eb025696208c646631772a45c241fc2971ef0c9866646d12dca12d476"
                .to_string(),
            risk_policy_hash: "d7aff21eb025696208c646631772a45c241fc2971ef0c9866646d12dca12d476"
                .to_string(),
            economic_control_epoch: 7,
            global_control_epoch: 8,
            route_control_epoch: 9,
            executor_code_hash: "e".repeat(64),
            contract_identity_hash: "f".repeat(64),
            wallet_gas_reserve_wei: 2,
            gas_reserve_floor_wei: 1,
            current_daily_loss_wei: 0,
            daily_loss_limit_wei: 1,
            observed_from: start,
            observed_until: start + ChronoDuration::seconds(1),
            created_at: start + ChronoDuration::seconds(2),
            expires_at: start + ChronoDuration::minutes(5),
            candidate_evidence_hashes: vec!["1".repeat(64)],
        }
    }

    #[test]
    fn evidence_start_accepts_only_the_exact_fail_closed_deploy() {
        let current = state(EconomicPhase::DisarmedDeploy);
        let authority = closed_authority();
        assert!(validate_evidence_start(
            &current,
            &"a".repeat(40),
            &format!("sha256:{}", "b".repeat(64)),
            "route",
            &"c".repeat(64),
            &"d".repeat(64),
            &authority,
        )
        .is_ok());

        let repeated = state(EconomicPhase::DisarmedEvidence);
        assert!(validate_evidence_start(
            &repeated,
            &"a".repeat(40),
            &format!("sha256:{}", "b".repeat(64)),
            "route",
            &"c".repeat(64),
            &"d".repeat(64),
            &authority,
        )
        .is_err());

        for (release, digest, route, universe, policy) in [
            (
                "0".repeat(40),
                format!("sha256:{}", "b".repeat(64)),
                "route".to_string(),
                "c".repeat(64),
                "d".repeat(64),
            ),
            (
                "a".repeat(40),
                format!("sha256:{}", "0".repeat(64)),
                "route".to_string(),
                "c".repeat(64),
                "d".repeat(64),
            ),
            (
                "a".repeat(40),
                format!("sha256:{}", "b".repeat(64)),
                "other-route".to_string(),
                "c".repeat(64),
                "d".repeat(64),
            ),
            (
                "a".repeat(40),
                format!("sha256:{}", "b".repeat(64)),
                "route".to_string(),
                "0".repeat(64),
                "d".repeat(64),
            ),
            (
                "a".repeat(40),
                format!("sha256:{}", "b".repeat(64)),
                "route".to_string(),
                "c".repeat(64),
                "0".repeat(64),
            ),
        ] {
            assert!(validate_evidence_start(
                &current, &release, &digest, &route, &universe, &policy, &authority,
            )
            .is_err());
        }
    }

    #[test]
    fn evidence_start_rejects_every_open_or_active_authority() {
        let current = state(EconomicPhase::DisarmedDeploy);
        let validate = |authority: &EvidenceAuthorityState| {
            validate_evidence_start(
                &current,
                &"a".repeat(40),
                &format!("sha256:{}", "b".repeat(64)),
                "route",
                &"c".repeat(64),
                &"d".repeat(64),
                authority,
            )
        };
        let mut variants = Vec::new();
        for index in 0..7 {
            let mut authority = closed_authority();
            match index {
                0 => authority.legacy_armed = true,
                1 => authority.legacy_kill_switch = false,
                2 => authority.global_armed = true,
                3 => authority.global_kill_switch = false,
                4 => authority.global_disarmed = false,
                5 => authority.route_enabled = true,
                6 => authority.route_kill_switch = false,
                _ => unreachable!(),
            }
            variants.push(authority);
        }
        assert!(variants
            .iter()
            .all(|authority| validate(authority).is_err()));

        let mut active = closed_authority();
        active.active_attempts = 1;
        assert!(validate(&active).is_err());
        let mut unresolved = closed_authority();
        unresolved.unresolved_receipts = 1;
        assert!(validate(&unresolved).is_err());
    }

    #[test]
    fn readiness_requires_evidence_phase_current_bindings_and_post_transition_window() {
        let binding = readiness();
        let mut evidence = state(EconomicPhase::DisarmedEvidence);
        evidence.route_universe_hash = Some(binding.route_universe_hash.clone());
        assert!(validate_readiness_against_evidence(&evidence, &binding).is_ok());

        let mut reverse = binding.clone();
        reverse.route_fingerprint = REVERSE_ROUTE_FINGERPRINT.to_string();
        reverse.route_policy_hash =
            "36da85c0fd07e5d3a12726582b20c84d81cfbd2d1d982da8237d3b5cf38b83d5".to_string();
        reverse
            .risk_policy_hash
            .clone_from(&reverse.route_policy_hash);
        assert!(validate_readiness_against_evidence(&evidence, &reverse).is_ok());

        let deploy = state(EconomicPhase::DisarmedDeploy);
        assert!(validate_readiness_against_evidence(&deploy, &binding).is_err());

        let mut stale = binding.clone();
        stale.observed_from = evidence.updated_at - ChronoDuration::seconds(1);
        assert!(validate_readiness_against_evidence(&evidence, &stale).is_err());

        let mut wrong_epoch = binding.clone();
        wrong_epoch.economic_control_epoch += 1;
        assert!(validate_readiness_against_evidence(&evidence, &wrong_epoch).is_err());

        for field in 0..7 {
            let mut wrong = binding.clone();
            match field {
                0 => wrong.release_sha = "0".repeat(40),
                1 => wrong.engine_image_digest = format!("sha256:{}", "0".repeat(64)),
                2 => wrong.route_fingerprint = "other-route".to_string(),
                3 => wrong.route_universe_hash = "0".repeat(64),
                4 => wrong.route_policy_hash = "0".repeat(64),
                5 => wrong.risk_policy_hash = "0".repeat(64),
                6 => wrong.executor_code_hash = "0".repeat(64),
                _ => unreachable!(),
            }
            assert!(validate_readiness_against_evidence(&evidence, &wrong).is_err());
        }
    }

    #[test]
    fn revenue_lane_size_authority_must_track_the_economic_level_exactly() {
        let expected = SizeLevel::L2.amount_wei();
        let active = vec![
            ("aave_liquidation".to_string(), true, false, expected),
            ("atlas_solver".to_string(), true, false, expected),
        ];
        assert_eq!(
            validate_revenue_lane_size_authority(&active, expected).unwrap(),
            2
        );

        let closed = vec![
            ("aave_liquidation".to_string(), false, true, 1),
            ("atlas_solver".to_string(), false, true, 2),
        ];
        assert_eq!(
            validate_revenue_lane_size_authority(&closed, expected).unwrap(),
            0
        );

        let mut divergent_size = active.clone();
        divergent_size[1].3 = SizeLevel::L4.amount_wei();
        assert!(validate_revenue_lane_size_authority(&divergent_size, expected).is_err());

        let mut partially_armed = active;
        partially_armed[1].1 = false;
        partially_armed[1].2 = true;
        assert!(validate_revenue_lane_size_authority(&partially_armed, expected).is_err());
    }

    #[test]
    fn max_reviewed_operator_authority_requires_every_closed_state_gate() {
        assert_eq!(
            SET_REVENUE_SIZE_MAX_REVIEWED_ACK,
            "SET_MAX_REVIEWED_LIVE_SIZE_42161"
        );
        let current = state(EconomicPhase::DisarmedEvidence);
        let valid = max_reviewed_authority_facts();
        assert!(
            validate_max_reviewed_authority_transition(&current, &"a".repeat(40), &valid).is_ok()
        );

        let mut variants = Vec::new();
        let mut wrong_input = valid.clone();
        wrong_input.current_input_wei += 1;
        variants.push(wrong_input);
        let mut wrong_maximum = valid.clone();
        wrong_maximum.maximum_reviewed_input_wei -= 1;
        variants.push(wrong_maximum);
        let mut armed_lane = valid.clone();
        armed_lane.revenue_lanes[0].1 = true;
        armed_lane.revenue_lanes[0].2 = false;
        variants.push(armed_lane);
        let mut generic_open = valid.clone();
        generic_open.generic_controls_closed = false;
        variants.push(generic_open);
        let mut locked = valid.clone();
        locked.submission_lock_free = false;
        variants.push(locked);
        let mut active = valid.clone();
        active.active_attempts = 1;
        variants.push(active);
        let mut unresolved = valid.clone();
        unresolved.unresolved_submissions = 1;
        variants.push(unresolved);
        let mut atlas = valid.clone();
        atlas.active_atlas_requests = 1;
        variants.push(atlas);
        let mut loss_limited = valid;
        loss_limited.current_daily_loss = 1;
        variants.push(loss_limited);
        assert!(variants.iter().all(|facts| {
            validate_max_reviewed_authority_transition(&current, &"a".repeat(40), facts).is_err()
        }));

        let wrong_phase = state(EconomicPhase::LiveCanaryMin);
        assert!(validate_max_reviewed_authority_transition(
            &wrong_phase,
            &"a".repeat(40),
            &max_reviewed_authority_facts(),
        )
        .is_err());
        assert!(validate_max_reviewed_authority_transition(
            &current,
            &"b".repeat(40),
            &max_reviewed_authority_facts(),
        )
        .is_err());
    }

    #[test]
    fn max_reviewed_operator_requires_fresh_exact_single_primary_readiness() {
        let now_millis = 1_800_000_000_000;
        let valid = hunter_readiness();
        assert!(validate_hunter_readiness(&valid, now_millis).is_ok());

        for field in [
            "ok",
            "hunting_health",
            "exact_execution_readiness",
            "atlas_connected",
        ] {
            let mut invalid = valid.clone();
            invalid[field] = Value::Bool(false);
            assert!(validate_hunter_readiness(&invalid, now_millis).is_err());
        }
        let mut circuit_open = valid.clone();
        circuit_open["provider_circuit_open_until_unix_millis"] =
            Value::Number((now_millis + 1).into());
        assert!(validate_hunter_readiness(&circuit_open, now_millis).is_err());
        let mut recovering = valid.clone();
        recovering["provider_recovery_state"] = Value::String("recovering".to_string());
        assert!(validate_hunter_readiness(&recovering, now_millis).is_err());
        let mut confirmation = valid.clone();
        confirmation["confirmation_provider_id"] = Value::String("forbidden".to_string());
        assert!(validate_hunter_readiness(&confirmation, now_millis).is_err());
        let mut no_primary = valid;
        no_primary["last_primary_exact_at"] = Value::Null;
        assert!(validate_hunter_readiness(&no_primary, now_millis).is_err());
    }

    #[test]
    fn repeated_current_provider_failures_require_an_open_circuit_before_fail_close() {
        let now_millis = Utc::now().timestamp_millis();
        for reason in [
            "provider_disagreement",
            "provider_unavailable",
            "provider_timeout",
            "provider_rate_limited",
        ] {
            let mut payload = hunter_readiness();
            payload["degraded_reason"] = Value::String(reason.to_string());
            payload["provider_recovery_state"] = Value::String("recovering".to_string());
            payload["provider_current_class_failure_streak"] = json!(2);
            payload["provider_circuit_open_until_unix_millis"] = json!(now_millis + 60_000);
            payload["provider_degraded_since_unix_millis"] =
                json!(now_millis - REVENUE_PROVIDER_FAILURE_MINIMUM_DURATION);
            assert!(persistent_hunter_provider_failure(&payload, now_millis).unwrap());

            payload["provider_degraded_since_unix_millis"] = json!(now_millis - 1_000);
            assert!(!persistent_hunter_provider_failure(&payload, now_millis).unwrap());
            payload["provider_degraded_since_unix_millis"] =
                json!(now_millis - REVENUE_PROVIDER_FAILURE_MINIMUM_DURATION);

            payload["provider_current_class_failure_streak"] = json!(1);
            assert!(!persistent_hunter_provider_failure(&payload, now_millis).unwrap());
            payload["provider_current_class_failure_streak"] = json!(2);
            payload["provider_circuit_open_until_unix_millis"] = json!(now_millis);
            assert!(!persistent_hunter_provider_failure(&payload, now_millis).unwrap());
        }
        let mut budget = hunter_readiness();
        budget["degraded_reason"] = Value::String("gateway_budget_exhausted".to_string());
        budget["provider_recovery_state"] = Value::String("recovering".to_string());
        budget["provider_current_class_failure_streak"] = json!(3);
        budget["provider_circuit_open_until_unix_millis"] = json!(now_millis + 60_000);
        assert!(!persistent_hunter_provider_failure(&budget, now_millis).unwrap());
    }

    #[test]
    fn provider_recovery_requires_three_fresh_ordered_primary_samples() {
        let now = Utc::now();
        let mut payload = hunter_readiness();
        payload["degraded_reason"] = Value::String(String::new());
        payload["provider_recovery_state"] = Value::String("ready".to_string());
        payload["provider_circuit_open_until_unix_millis"] = json!(0);
        payload["provider_recovery_samples"] = json!([
            {"observed_at": (now-ChronoDuration::seconds(3)).to_rfc3339(), "primary_provider":"production-nownodes-arbitrum", "confirmation":null, "quorum":1},
            {"observed_at": (now-ChronoDuration::seconds(2)).to_rfc3339(), "primary_provider":"production-nownodes-arbitrum", "confirmation":null, "quorum":1},
            {"observed_at": (now-ChronoDuration::seconds(1)).to_rfc3339(), "primary_provider":"production-nownodes-arbitrum", "confirmation":null, "quorum":1}
        ]);
        assert_eq!(provider_recovery_samples(&payload).unwrap().len(), 3);

        payload["provider_recovery_samples"][2]["confirmation"] =
            Value::String("forbidden".to_string());
        assert!(provider_recovery_samples(&payload).is_err());
        payload["provider_recovery_samples"] = json!([]);
        assert!(provider_recovery_samples(&payload).is_err());
    }

    fn valid_provider_recovery_binding(
        failure_at: DateTime<Utc>,
        samples: &[(DateTime<Utc>, String)],
    ) -> (EconomicState, ProviderRecoveryTransitionBinding) {
        let mut previous = state(EconomicPhase::DisarmedFailure);
        previous.level = SizeLevel::MaxReviewed;
        (
            previous,
            ProviderRecoveryTransitionBinding {
                reason: "provider_timeout".to_string(),
                failure_epoch: 7,
                failure_at,
                failure_release: "a".repeat(40),
                restore_phase: "DISARMED_EVIDENCE".to_string(),
                restore_size: "MAX_REVIEWED".to_string(),
                durable_sample_count: 3,
                durable_samples: samples.to_vec(),
            },
        )
    }

    #[test]
    fn provider_recovery_transition_is_allowlisted_epoch_bound_and_post_failure() {
        let failure_at = evidence_time();
        let samples = (1..=3)
            .map(|offset| {
                (
                    failure_at + ChronoDuration::seconds(offset),
                    "production-nownodes-arbitrum".to_string(),
                )
            })
            .collect::<Vec<_>>();
        let (previous, valid) = valid_provider_recovery_binding(failure_at, &samples);
        assert!(validate_provider_recovery_transition_binding(
            &previous,
            &"a".repeat(40),
            &valid,
            &samples,
        )
        .is_ok());

        let mut variants = Vec::new();
        let mut wrong_reason = valid.clone();
        wrong_reason.reason = "daily_loss_budget".to_string();
        variants.push(wrong_reason);
        let mut wrong_epoch = valid.clone();
        wrong_epoch.failure_epoch += 1;
        variants.push(wrong_epoch);
        let mut wrong_release = valid.clone();
        wrong_release.failure_release = "b".repeat(40);
        variants.push(wrong_release);
        let mut wrong_phase = valid.clone();
        wrong_phase.restore_phase = "LIVE_MAX_REVIEWED".to_string();
        variants.push(wrong_phase);
        let mut wrong_size = valid.clone();
        wrong_size.restore_size = "MIN".to_string();
        variants.push(wrong_size);
        let mut wrong_count = valid.clone();
        wrong_count.durable_sample_count = 2;
        variants.push(wrong_count);
        let mut stale = valid.clone();
        stale.durable_samples[0].0 = failure_at;
        variants.push(stale);
        assert!(variants.iter().all(|binding| {
            validate_provider_recovery_transition_binding(
                &previous,
                &"a".repeat(40),
                binding,
                &binding.durable_samples,
            )
            .is_err()
        }));

        let mut wrong_previous = previous;
        wrong_previous.phase = EconomicPhase::DisarmedEvidence;
        assert!(validate_provider_recovery_transition_binding(
            &wrong_previous,
            &"a".repeat(40),
            &valid,
            &samples,
        )
        .is_err());
    }

    fn valid_provider_recovery_runtime_facts() -> ProviderRecoveryRuntimeFacts {
        ProviderRecoveryRuntimeFacts {
            lanes: vec![
                (
                    "aave_liquidation".to_string(),
                    false,
                    true,
                    MAXIMUM_REVIEWED_INPUT_WEI,
                ),
                (
                    "atlas_solver".to_string(),
                    false,
                    true,
                    MAXIMUM_REVIEWED_INPUT_WEI,
                ),
            ],
            generic_closed: true,
            active_attempts: 0,
            unresolved_submissions: 0,
            active_atlas: 0,
            lock_free: true,
            current_daily_loss: 0,
            daily_loss_limit: 1,
        }
    }

    #[test]
    fn provider_recovery_runtime_rejects_every_open_work_or_policy_gate() {
        assert!(
            validate_provider_recovery_runtime_facts(&valid_provider_recovery_runtime_facts())
                .is_ok()
        );
        let mut variants = Vec::new();
        let mut missing_lane = valid_provider_recovery_runtime_facts();
        missing_lane.lanes.pop();
        variants.push(missing_lane);
        let mut partial = valid_provider_recovery_runtime_facts();
        partial.lanes[0].1 = true;
        variants.push(partial);
        let mut kill_open = valid_provider_recovery_runtime_facts();
        kill_open.lanes[1].2 = false;
        variants.push(kill_open);
        let mut wrong_maximum = valid_provider_recovery_runtime_facts();
        wrong_maximum.lanes[0].3 -= 1;
        variants.push(wrong_maximum);
        let mut generic_open = valid_provider_recovery_runtime_facts();
        generic_open.generic_closed = false;
        variants.push(generic_open);
        let mut attempt = valid_provider_recovery_runtime_facts();
        attempt.active_attempts = 1;
        variants.push(attempt);
        let mut unresolved = valid_provider_recovery_runtime_facts();
        unresolved.unresolved_submissions = 1;
        variants.push(unresolved);
        let mut atlas = valid_provider_recovery_runtime_facts();
        atlas.active_atlas = 1;
        variants.push(atlas);
        let mut locked = valid_provider_recovery_runtime_facts();
        locked.lock_free = false;
        variants.push(locked);
        let mut exhausted = valid_provider_recovery_runtime_facts();
        exhausted.current_daily_loss = exhausted.daily_loss_limit;
        variants.push(exhausted);
        assert!(variants
            .iter()
            .all(|facts| validate_provider_recovery_runtime_facts(facts).is_err()));
    }

    #[test]
    fn provider_recovery_owner_evidence_requires_exact_release_configuration_and_maximum() {
        let valid = json!({
            "release_sha": "a".repeat(40),
            "final_state": {
                "paused": false,
                "configuration_complete": true,
                "maximum_input_amount": MAXIMUM_REVIEWED_INPUT_WEI.to_string()
            }
        });
        assert!(provider_recovery_owner_state(&valid, &"a".repeat(40)).is_ok());
        for (field, value) in [
            ("release_sha", Value::String("b".repeat(40))),
            ("configuration_complete", Value::Bool(false)),
            ("maximum_input_amount", Value::String("1".to_string())),
        ] {
            let mut invalid = valid.clone();
            if field == "release_sha" {
                invalid[field] = value;
            } else {
                invalid["final_state"][field] = value;
            }
            assert!(provider_recovery_owner_state(&invalid, &"a".repeat(40)).is_err());
        }
    }
}
