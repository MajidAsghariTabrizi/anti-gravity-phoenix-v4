use chrono::{SecondsFormat, Utc};
use ethabi::{ParamType, Token};
use phoenix_live_executor::model::{CanonicalAddress, ExecutionRequest, ValidatedLeg};
use phoenix_live_executor::rpc::{ExecutionRpc, HttpExecutionRpc};
use phoenix_live_executor::{
    APPROVAL_POLICY_VERSION, CURRENT_ROUTE_FINGERPRINT, REQUEST_SCHEMA_VERSION,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::io;
use std::time::Duration;
use url::Url;
use uuid::Uuid;

const POLICY: &str = include_str!("../../config/phoenix-route-policy-v1.json");
const MIGRATIONS: [(&str, &str); 4] = [
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
];
const ACTIVATE_ACK: &str = "ACTIVATE_AUTONOMOUS_LIVE_42161";
const DISARM_ACK: &str = "DISARM_AUTONOMOUS_LIVE_42161";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    if let Err(error) = run().await {
        eprintln!("AUTONOMOUS_CONTROL_FAILED: {error}");
        return Err(io::Error::other("autonomous control failed").into());
    }
    Ok(())
}

async fn run() -> Result<(), &'static str> {
    let command = env::args().nth(1).ok_or("command is required")?;
    let dsn = required("POSTGRES_DSN")?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&dsn)
        .await
        .map_err(|_| "database connection failed")?;
    match command.as_str() {
        "migrate" => migrate(&pool).await?,
        "activate" => activate(&pool).await?,
        "disarm" => disarm(&pool).await?,
        "status" => status(&pool).await?,
        "reconciliation-status" => reconciliation_status(&pool).await?,
        "preflight" => preflight().await?,
        "owner-plan" => owner_plan().await?,
        _ => return Err("unsupported command"),
    }
    Ok(())
}

async fn preflight() -> Result<(), &'static str> {
    let primary_url =
        Url::parse(&required("PRODUCTION_RPC_URL")?).map_err(|_| "primary RPC URL is invalid")?;
    let secondary_url =
        Url::parse(&required("SECONDARY_RPC_URL")?).map_err(|_| "secondary RPC URL is invalid")?;
    if primary_url == secondary_url {
        return Err("primary and secondary RPC providers are not independent");
    }
    let allowlist = required("LIVE_EXECUTOR_RPC_ALLOWLIST")?
        .split(',')
        .map(|value| Url::parse(value).map_err(|_| "RPC allowlist is invalid"))
        .collect::<Result<Vec<_>, _>>()?;
    let primary = HttpExecutionRpc::new_production(primary_url, &allowlist)
        .map_err(|_| "primary RPC is not allowlisted")?;
    let secondary = HttpExecutionRpc::new_production(secondary_url, &allowlist)
        .map_err(|_| "secondary RPC is not allowlisted")?;
    if primary
        .chain_id()
        .await
        .map_err(|_| "primary chain identity is unavailable")?
        != 42_161
        || secondary
            .chain_id()
            .await
            .map_err(|_| "secondary chain identity is unavailable")?
            != 42_161
    {
        return Err("RPC chain identity mismatch");
    }
    let wallet = CanonicalAddress::parse(&required("LIVE_EXECUTOR_WALLET_ADDRESS")?)
        .map_err(|_| "wallet address is invalid")?;
    let executor = CanonicalAddress::parse(&required("LIVE_EXECUTOR_EXECUTOR_ADDRESS")?)
        .map_err(|_| "executor address is invalid")?;
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
        return Err("wallet has no native gas balance");
    }
    let (owner, flash_provider) = primary
        .executor_owner_and_flash_provider(executor)
        .await
        .map_err(|_| "executor ownership state is unavailable")?;
    if owner != expected_owner || flash_provider != expected_flash_provider {
        return Err("executor owner or flash provider mismatch");
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
        return Err("route path is inconsistent");
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
        return Err("reviewed router set is invalid");
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
            return Err("executor configuration is not LIVE-ready");
        }
    }
    println!(
        "AUTONOMOUS_PREFLIGHT_OK: chain=42161 wallet_gas=positive executor_state=ready providers=2"
    );
    Ok(())
}

async fn owner_plan() -> Result<(), &'static str> {
    let primary_url =
        Url::parse(&required("PRODUCTION_RPC_URL")?).map_err(|_| "primary RPC URL is invalid")?;
    let allowlist = required("LIVE_EXECUTOR_RPC_ALLOWLIST")?
        .split(',')
        .map(|value| Url::parse(value).map_err(|_| "RPC allowlist is invalid"))
        .collect::<Result<Vec<_>, _>>()?;
    let primary = HttpExecutionRpc::new_production(primary_url, &allowlist)
        .map_err(|_| "primary RPC is not allowlisted")?;
    if primary
        .chain_id()
        .await
        .map_err(|_| "primary chain identity is unavailable")?
        != 42_161
    {
        return Err("RPC chain identity mismatch");
    }
    let wallet = CanonicalAddress::parse(&required("LIVE_EXECUTOR_WALLET_ADDRESS")?)
        .map_err(|_| "wallet address is invalid")?;
    let executor = CanonicalAddress::parse(&required("LIVE_EXECUTOR_EXECUTOR_ADDRESS")?)
        .map_err(|_| "executor address is invalid")?;
    let expected_owner = CanonicalAddress::parse(&required("LIVE_EXECUTOR_EXPECTED_OWNER")?)
        .map_err(|_| "expected owner is invalid")?;
    let expected_flash_provider =
        CanonicalAddress::parse(&required("LIVE_EXECUTOR_EXPECTED_FLASH_PROVIDER")?)
            .map_err(|_| "expected flash provider is invalid")?;
    let expected_code_hash = required("LIVE_EXECUTOR_EXECUTOR_CODE_HASH")?;
    let maximum_input = required_u128("LIVE_EXECUTOR_MAX_INPUT_AMOUNT")?;
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
    if pools.len() + 1 != token_path.len()
        || factories.len() != pools.len()
        || fees.len() != pools.len()
    {
        return Err("route path is inconsistent");
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
                zero_for_one: token_path[index].as_bytes() < token_path[index + 1].as_bytes(),
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
        return Err("reviewed router set is invalid");
    }

    let mut snapshots = Vec::with_capacity(routers.len());
    for router in &routers {
        let snapshot = primary
            .executor_configuration_snapshot(executor, wallet, token_path[0], *router, &legs)
            .await
            .map_err(|_| "executor configuration state is unavailable")?;
        if snapshot.runtime_code_hash != expected_code_hash
            || snapshot.owner != Some(expected_owner)
            || snapshot.flash_provider != Some(expected_flash_provider)
        {
            return Err("executor immutable identity or ownership state mismatch");
        }
        snapshots.push(snapshot);
    }
    let first = snapshots.first().ok_or("reviewed router set is invalid")?;
    let mut transactions = Vec::new();
    if !first.searcher_authorized {
        transactions.push(owner_transaction(
            executor,
            "authorize autonomous searcher",
            "setSearcher",
            &[ParamType::Address, ParamType::Bool],
            &[address_token(wallet), Token::Bool(true)],
        ));
    }
    if !first.asset_approved {
        transactions.push(owner_transaction(
            executor,
            "approve settlement asset",
            "setAsset",
            &[ParamType::Address, ParamType::Bool],
            &[address_token(token_path[0]), Token::Bool(true)],
        ));
    }
    for (router, snapshot) in routers.iter().zip(&snapshots) {
        if !snapshot.router_approved {
            transactions.push(owner_transaction(
                executor,
                "approve reviewed router",
                "setRouter",
                &[ParamType::Address, ParamType::Bool],
                &[address_token(*router), Token::Bool(true)],
            ));
        }
    }
    for (index, leg) in legs.iter().enumerate() {
        if !first.factories_approved[index] {
            let factory = leg.factory.ok_or("route factory is invalid")?;
            if !legs[..index]
                .iter()
                .any(|prior| prior.factory == Some(factory))
            {
                transactions.push(owner_transaction(
                    executor,
                    "approve reviewed factory",
                    "setFactory",
                    &[ParamType::Address, ParamType::Bool],
                    &[address_token(factory), Token::Bool(true)],
                ));
            }
        }
    }
    for (index, leg) in legs.iter().enumerate() {
        if !first.pools_approved[index] {
            let factory = leg.factory.ok_or("route factory is invalid")?;
            let (token0, token1) = ordered_pair(leg.token_in, leg.token_out);
            transactions.push(owner_transaction(
                executor,
                "approve reviewed pool",
                "approvePool",
                &[
                    ParamType::Address,
                    ParamType::Address,
                    ParamType::Address,
                    ParamType::Address,
                    ParamType::Uint(24),
                    ParamType::Bool,
                ],
                &[
                    address_token(leg.pool),
                    address_token(factory),
                    address_token(token0),
                    address_token(token1),
                    Token::Uint(leg.fee.into()),
                    Token::Bool(true),
                ],
            ));
        }
    }
    if first.maximum_input_amount != maximum_input {
        transactions.push(owner_transaction(
            executor,
            "set conservative maximum input",
            "setMaximumInputAmount",
            &[ParamType::Uint(256)],
            &[Token::Uint(maximum_input.into())],
        ));
    }
    if first.paused {
        transactions.push(owner_transaction(
            executor,
            "unpause executor after reviewed configuration",
            "setPaused",
            &[ParamType::Bool],
            &[Token::Bool(false)],
        ));
    }
    let status = if transactions.is_empty() {
        "ready"
    } else {
        "EXTERNAL_OWNER_AUTHORIZATION_REQUIRED"
    };
    let payload = json!({
        "schema": "phoenix.executor-owner-plan.v1",
        "status": status,
        "chain_id": 42161,
        "target": executor.to_string(),
        "value": "0",
        "expected_post_state": {
            "runtime_code_hash": expected_code_hash,
            "owner": expected_owner.to_string(),
            "flash_provider": expected_flash_provider.to_string(),
            "paused": false,
            "maximum_input_amount": maximum_input.to_string(),
            "authorized_searcher": wallet.to_string(),
            "approved_asset": token_path[0].to_string(),
            "approved_routers": routers.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "route_policy_hash": value_text(&policy, "policy_hash")?
        },
        "transactions": transactions,
        "verification_command": "autonomous-live-control preflight"
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).map_err(|_| "owner plan serialization failed")?
    );
    Ok(())
}

fn owner_transaction(
    target: CanonicalAddress,
    description: &'static str,
    name: &str,
    input_types: &[ParamType],
    arguments: &[Token],
) -> Value {
    let mut data = ethabi::short_signature(name, input_types).to_vec();
    data.extend(ethabi::encode(arguments));
    json!({
        "chain_id": 42161,
        "target": target.to_string(),
        "value": "0",
        "data": format!("0x{}", hex::encode(data)),
        "description": description
    })
}

fn address_token(address: CanonicalAddress) -> Token {
    Token::Address(primitive_types::H160::from_slice(address.as_bytes()))
}

fn ordered_pair(
    left: CanonicalAddress,
    right: CanonicalAddress,
) -> (CanonicalAddress, CanonicalAddress) {
    if left.as_bytes() < right.as_bytes() {
        (left, right)
    } else {
        (right, left)
    }
}

fn preflight_request(
    executor: CanonicalAddress,
    router: CanonicalAddress,
    maximum_input: u128,
    token_path: Vec<CanonicalAddress>,
    legs: Vec<ValidatedLeg>,
) -> Result<ExecutionRequest, &'static str> {
    let flash_asset = *token_path.first().ok_or("route token path is empty")?;
    let now = Utc::now();
    Ok(ExecutionRequest {
        id: Uuid::nil(),
        opportunity_id: Uuid::nil(),
        schema_version: REQUEST_SCHEMA_VERSION.to_string(),
        chain_id: 42_161,
        route_id: [0; 32],
        route_fingerprint: CURRENT_ROUTE_FINGERPRINT.to_string(),
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
    println!("AUTONOMOUS_MIGRATION_OK: phoenix.live-canary-schema.v4");
    Ok(())
}

async fn activate(pool: &PgPool) -> Result<(), &'static str> {
    if required("PHOENIX_AUTONOMOUS_ACTIVATION_ACK")? != ACTIVATE_ACK {
        return Err("activation acknowledgement is invalid");
    }
    require_schema(pool).await?;
    let policy: Value = serde_json::from_str(POLICY).map_err(|_| "route policy is invalid")?;
    verify_hash(
        &policy,
        "policy_hash",
        "route-policy",
        "phoenix.route-policy.v1",
    )?;
    if policy
        .get("enabled_for_autonomous_live")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err("route policy is not enabled for autonomous LIVE");
    }
    let configured_maximum = required_u128("LIVE_EXECUTOR_MAX_INPUT_AMOUNT")?;
    let policy_minimum = value_u128(&policy, "minimum_input_amount")?;
    let policy_maximum = value_u128(&policy, "maximum_input_amount")?;
    let maximum_input = configured_maximum.min(policy_maximum);
    if maximum_input < policy_minimum {
        return Err("configured maximum input is economically inert");
    }
    let daily_loss_limit = required_u128("LIVE_EXECUTOR_MAX_DAILY_LOSS_WEI")?;
    if daily_loss_limit == 0 {
        return Err("global daily loss limit is economically inert");
    }

    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| "database transaction failed")?;
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
        return Err("activation is blocked by an active execution attempt");
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
    let updated_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
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
        "current_size_level": "0.25x",
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
             updated_at = $1
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
             control_contract = $5, updated_at = $6
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
            $1, $2, true, false, '0.25x', $3::numeric, NULL,
            $4, NULL, $5, $6, $7
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
    transaction
        .commit()
        .await
        .map_err(|_| "activation commit failed")?;
    println!(
        "AUTONOMOUS_ACTIVATION_OK: chain=42161 route={} global_epoch={} route_epoch={}",
        route_fingerprint, global_epoch, route_epoch
    );
    Ok(())
}

async fn disarm(pool: &PgPool) -> Result<(), &'static str> {
    if required("PHOENIX_AUTONOMOUS_DISARM_ACK")? != DISARM_ACK {
        return Err("disarm acknowledgement is invalid");
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
    transaction
        .commit()
        .await
        .map_err(|_| "disarm commit failed")?;
    println!("AUTONOMOUS_DISARM_OK: reason={reason}");
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
    let payload = json!({
        "schema": "phoenix.autonomous-live-status.v1",
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
        }))
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
             WHERE version = 'phoenix.live-canary-schema.v4'
         )",
    )
    .fetch_one(pool)
    .await
    .map_err(|_| "schema inspection failed")?;
    if !installed {
        return Err("phoenix.live-canary-schema.v4 is not installed");
    }
    Ok(())
}

fn required(name: &str) -> Result<String, &'static str> {
    env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or("required environment is missing")
}

fn required_u128(name: &str) -> Result<u128, &'static str> {
    required(name)?
        .parse()
        .ok()
        .filter(|value| *value > 0)
        .ok_or("required numeric environment is invalid")
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
