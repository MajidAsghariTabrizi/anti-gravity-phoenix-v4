use phoenix_live_executor::config::transaction_signer_from_file;
use phoenix_live_executor::executor_rotation::{
    persist_provenance, persist_provenance_create_new, read_provenance,
    recover_existing_provenance, validate_creation_bytecode, validate_plan,
    AuthenticatedPhoenixExecutorRotationTransport, ProductionPhoenixExecutorRotationBackend,
    RotationOperator, RotationPlan, OLD_EXECUTOR,
};
use phoenix_live_executor::model::{CanonicalAddress, TransactionHash};
use phoenix_live_executor::rpc::{ExecutionRpc, HttpExecutionRpc, PhoenixExecutorMapping};
use phoenix_live_executor::store::PostgresExecutorStore;
use serde_json::json;
use std::{
    env, fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    process::ExitCode,
};
use url::Url;

fn failure(code: &'static str) -> ExitCode {
    eprintln!("{code}");
    ExitCode::from(1)
}

fn required(name: &'static str) -> Result<String, ExitCode> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| failure("ROTATION_ENVIRONMENT_INVALID"))
}

const RECOVERY_LEGACY_TOOLING_SHA: &str = "79c364f8aa56b6b6e27cd74cd2167e75a0b13610";
const RECOVERY_NEW_EXECUTOR: &str = "0x3d0c8340fc70616892635e1b6877475a2f915e95";
const RECOVERY_DEPLOYMENT_TX: &str =
    "0x51671df9c8ae4fa7fb7a1b821935b7e39988f0b8170321c3e93ccc084e594f64";
const RECOVERY_DEPLOYMENT_BLOCK: u64 = 495_235_786;

fn recovery_address(value: &str) -> Result<CanonicalAddress, ExitCode> {
    CanonicalAddress::parse(value).map_err(|_| failure("ROTATION_RECOVERY_CONTEXT_INVALID"))
}

fn production_rpc() -> Result<HttpExecutionRpc, ExitCode> {
    let endpoint_text = required("LIVE_EXECUTOR_RPC_URL")?;
    let allowlist_text = required("LIVE_EXECUTOR_RPC_ALLOWLIST")?;

    let endpoint = Url::parse(&endpoint_text).map_err(|_| failure("ROTATION_RPC_INVALID"))?;

    let allowlist = allowlist_text
        .split(',')
        .map(|item| Url::parse(item.trim()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| failure("ROTATION_RPC_INVALID"))?;

    if allowlist.len() != 1 || allowlist[0] != endpoint {
        return Err(failure("ROTATION_RPC_INVALID"));
    }

    let header_name = required("LIVE_EXECUTOR_RPC_HEADER_NAME")?;
    let header_file = required("LIVE_EXECUTOR_RPC_HEADER_FILE")?;

    HttpExecutionRpc::new_production_authenticated(endpoint, &allowlist, &header_name, &header_file)
        .map_err(|_| failure("ROTATION_RPC_INVALID"))
}

async fn verify_existing_config_read_only(
    rpc: &HttpExecutionRpc,
    executor: CanonicalAddress,
    plan: &RotationPlan,
) -> Result<(), ExitCode> {
    let core = rpc
        .phoenix_executor_core_snapshot(executor)
        .await
        .map_err(|_| failure("ROTATION_RECOVERY_RPC_FAILED"))?;

    if core.owner != Some(recovery_address(&plan.owner)?)
        || core.pending_owner.is_some()
        || core.flash_provider != Some(recovery_address(&plan.flash_provider)?)
        || core.atlas != Some(recovery_address(&plan.atlas)?)
        || core.weth != Some(recovery_address(&plan.weth)?)
        || core.paused
        || core.maximum_input_amount != plan.maximum_input_amount
    {
        return Err(failure("ROTATION_RECOVERY_CONFIG_MISMATCH"));
    }

    if !rpc
        .phoenix_executor_mapping(
            executor,
            PhoenixExecutorMapping::Searcher,
            recovery_address(&plan.searcher)?,
        )
        .await
        .map_err(|_| failure("ROTATION_RECOVERY_RPC_FAILED"))?
    {
        return Err(failure("ROTATION_RECOVERY_CONFIG_MISMATCH"));
    }

    for value in &plan.assets {
        if !rpc
            .phoenix_executor_mapping(
                executor,
                PhoenixExecutorMapping::Asset,
                recovery_address(value)?,
            )
            .await
            .map_err(|_| failure("ROTATION_RECOVERY_RPC_FAILED"))?
        {
            return Err(failure("ROTATION_RECOVERY_CONFIG_MISMATCH"));
        }
    }

    for value in &plan.routers {
        if !rpc
            .phoenix_executor_mapping(
                executor,
                PhoenixExecutorMapping::Router,
                recovery_address(value)?,
            )
            .await
            .map_err(|_| failure("ROTATION_RECOVERY_RPC_FAILED"))?
        {
            return Err(failure("ROTATION_RECOVERY_CONFIG_MISMATCH"));
        }
    }

    if !rpc
        .phoenix_executor_mapping(
            executor,
            PhoenixExecutorMapping::Factory,
            recovery_address(&plan.factory)?,
        )
        .await
        .map_err(|_| failure("ROTATION_RECOVERY_RPC_FAILED"))?
    {
        return Err(failure("ROTATION_RECOVERY_CONFIG_MISMATCH"));
    }

    for pool in &plan.pools {
        if !rpc
            .phoenix_executor_pool_matches(
                executor,
                recovery_address(&pool.address)?,
                recovery_address(&plan.factory)?,
                recovery_address(&pool.token0)?,
                recovery_address(&pool.token1)?,
                pool.fee,
            )
            .await
            .map_err(|_| failure("ROTATION_RECOVERY_RPC_FAILED"))?
        {
            return Err(failure("ROTATION_RECOVERY_CONFIG_MISMATCH"));
        }
    }

    Ok(())
}

async fn recover_existing(
    current_plan_path: &str,
    legacy_plan_path: &str,
    legacy_state_path: &str,
    current_state_path: &str,
) -> Result<serde_json::Value, ExitCode> {
    let current_plan = load_plan(current_plan_path)?;
    let legacy_plan = load_plan(legacy_plan_path)?;

    if legacy_plan.source_sha != RECOVERY_LEGACY_TOOLING_SHA {
        return Err(failure("ROTATION_RECOVERY_LEGACY_SOURCE_INVALID"));
    }

    let legacy_state = Path::new(legacy_state_path);
    let current_state = Path::new(current_state_path);

    match fs::symlink_metadata(current_state) {
        Ok(_) => return Err(failure("ROTATION_RECOVERY_TARGET_EXISTS")),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(_) => return Err(failure("ROTATION_RECOVERY_TARGET_INVALID")),
    }

    let legacy = read_provenance(legacy_state, &legacy_plan)
        .map_err(|_| failure("ROTATION_RECOVERY_LEGACY_INVALID"))?;

    if legacy.tooling_source_sha != RECOVERY_LEGACY_TOOLING_SHA
        || legacy.new_executor != RECOVERY_NEW_EXECUTOR
        || legacy.deployment_tx_hash != RECOVERY_DEPLOYMENT_TX
        || legacy.deployment_block_number != RECOVERY_DEPLOYMENT_BLOCK
    {
        return Err(failure("ROTATION_RECOVERY_INCIDENT_IDENTITY_MISMATCH"));
    }

    let recovered = recover_existing_provenance(&current_plan, &legacy_plan, &legacy)
        .map_err(|_| failure("ROTATION_RECOVERY_PROVENANCE_INVALID"))?;

    let rpc = production_rpc()?;

    if rpc
        .chain_id()
        .await
        .map_err(|_| failure("ROTATION_RECOVERY_RPC_FAILED"))?
        != current_plan.chain_id
    {
        return Err(failure("ROTATION_RECOVERY_CHAIN_MISMATCH"));
    }

    let old_executor = recovery_address(&current_plan.old_executor)?;
    let new_executor = recovery_address(&legacy.new_executor)?;

    let old_hash = rpc
        .runtime_code_hash(old_executor)
        .await
        .map_err(|_| failure("ROTATION_RECOVERY_RPC_FAILED"))?;
    if old_hash != current_plan.old_runtime_sha256 {
        return Err(failure("ROTATION_RECOVERY_OLD_RUNTIME_MISMATCH"));
    }

    let new_hash = rpc
        .runtime_code_hash(new_executor)
        .await
        .map_err(|_| failure("ROTATION_RECOVERY_RPC_FAILED"))?;
    if new_hash != current_plan.expected_new_runtime_sha256 {
        return Err(failure("ROTATION_RECOVERY_NEW_RUNTIME_MISMATCH"));
    }

    let deployment_tx = TransactionHash::parse(&legacy.deployment_tx_hash)
        .map_err(|_| failure("ROTATION_RECOVERY_DEPLOYMENT_INVALID"))?;

    let deployment_receipt = rpc
        .transaction_receipt(deployment_tx)
        .await
        .map_err(|_| failure("ROTATION_RECOVERY_RPC_FAILED"))?
        .ok_or_else(|| failure("ROTATION_RECOVERY_DEPLOYMENT_MISSING"))?;

    if deployment_receipt.status != 1
        || deployment_receipt.transaction_hash != deployment_tx
        || deployment_receipt.contract_address != Some(new_executor)
        || deployment_receipt.block_number != legacy.deployment_block_number
    {
        return Err(failure("ROTATION_RECOVERY_DEPLOYMENT_MISMATCH"));
    }

    if legacy.config_tx_hashes.len() != 14 {
        return Err(failure("ROTATION_RECOVERY_CONFIG_TX_COUNT_INVALID"));
    }

    for raw_hash in &legacy.config_tx_hashes {
        let tx = TransactionHash::parse(raw_hash)
            .map_err(|_| failure("ROTATION_RECOVERY_CONFIG_TX_INVALID"))?;

        let receipt = rpc
            .transaction_receipt(tx)
            .await
            .map_err(|_| failure("ROTATION_RECOVERY_RPC_FAILED"))?
            .ok_or_else(|| failure("ROTATION_RECOVERY_CONFIG_TX_MISSING"))?;

        if receipt.status != 1 || receipt.transaction_hash != tx {
            return Err(failure("ROTATION_RECOVERY_CONFIG_TX_FAILED"));
        }

        if rpc
            .transaction_input(tx, new_executor)
            .await
            .map_err(|_| failure("ROTATION_RECOVERY_RPC_FAILED"))?
            .is_none()
        {
            return Err(failure("ROTATION_RECOVERY_CONFIG_TX_TARGET_INVALID"));
        }
    }

    verify_existing_config_read_only(&rpc, new_executor, &current_plan).await?;

    persist_provenance_create_new(current_state, &recovered)
        .map_err(|_| failure("ROTATION_RECOVERY_PERSIST_FAILED"))?;

    let persisted = read_provenance(current_state, &current_plan)
        .map_err(|_| failure("ROTATION_RECOVERY_POSTWRITE_INVALID"))?;

    if persisted != recovered {
        return Err(failure("ROTATION_RECOVERY_POSTWRITE_MISMATCH"));
    }

    Ok(json!({
        "schema": "phoenix.executor-rotation-recovery.v1",
        "status": "recovered-existing",
        "legacy_tooling_sha": legacy.tooling_source_sha,
        "current_source_sha": current_plan.source_sha,
        "new_executor": persisted.new_executor,
        "deployment_tx_hash": persisted.deployment_tx_hash,
        "deployment_block_number": persisted.deployment_block_number,
        "config_tx_count": persisted.config_tx_hashes.len(),
        "config_verified": persisted.config_verified,
        "cutover_started": persisted.cutover_started,
        "cutover_completed": persisted.cutover_completed,
        "transaction_submitted": false
    }))
}

fn load_plan(path: &str) -> Result<RotationPlan, ExitCode> {
    let bytes = fs::read(path).map_err(|_| failure("ROTATION_PLAN_INVALID"))?;
    let plan: RotationPlan =
        serde_json::from_slice(&bytes).map_err(|_| failure("ROTATION_PLAN_INVALID"))?;
    validate_plan(&plan).map_err(|_| failure("ROTATION_PLAN_INVALID"))?;
    Ok(plan)
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("");
    if mode == "validate" {
        if args.len() != 4 {
            return failure("ROTATION_ARGUMENTS_INVALID");
        }
        let plan = match load_plan(&args[2]) {
            Ok(value) => value,
            Err(code) => return code,
        };
        if validate_creation_bytecode(&plan, &args[3]).is_err() {
            return failure("ROTATION_PLAN_INVALID");
        }
        println!(
            "{}",
            json!({
                "chain_id": plan.chain_id,
                "maximum_input_amount": plan.maximum_input_amount.to_string(),
                "old_executor": plan.old_executor,
                "schema": plan.schema,
                "status": "validated"
            })
        );
        return ExitCode::SUCCESS;
    }
    if mode == "drain-store" {
        if args.len() != 4 {
            return failure("ROTATION_ARGUMENTS_INVALID");
        }
        let plan = match load_plan(&args[2]) {
            Ok(value) => value,
            Err(code) => return code,
        };
        let mut provenance = match read_provenance(PathBuf::from(&args[3]).as_path(), &plan) {
            Ok(value) if value.config_verified && value.pre_cutover_spl_absent => value,
            _ => return failure("ROTATION_PREPARE_REQUIRED"),
        };
        let dsn = match required("POSTGRES_DSN") {
            Ok(value) => value,
            Err(code) => return code,
        };
        let store = match PostgresExecutorStore::connect(&dsn).await {
            Ok(value) => value,
            Err(_) => return failure("ROTATION_STORE_INVALID"),
        };
        let drained = match store.drain_executor_identity(OLD_EXECUTOR).await {
            Ok(value) => value,
            Err(_) => return failure("ROTATION_DRAIN_REJECTED"),
        };
        provenance.old_bound_work_drained = drained.active_attempts == 0
            && drained.unresolved_submissions == 0
            && drained.active_atlas_requests == 0
            && drained.submission_lock_free;
        provenance.fenced_old_requests = match drained
            .fenced_requests
            .checked_add(drained.fenced_atlas_requests)
        {
            Some(value) => value,
            None => return failure("ROTATION_DRAIN_REJECTED"),
        };
        if !provenance.old_bound_work_drained
            || persist_provenance(PathBuf::from(&args[3]).as_path(), &provenance).is_err()
        {
            return failure("ROTATION_DRAIN_REJECTED");
        }
        println!(
            "{}",
            json!({
                "fenced_atlas_requests": drained.fenced_atlas_requests,
                "fenced_requests": drained.fenced_requests,
                "status": "drained"
            })
        );
        return ExitCode::SUCCESS;
    }

    // Recovery executes before creation bytecode and signer loading.
    if mode == "recover-existing" {
        if args.len() != 6 {
            return failure("ROTATION_ARGUMENTS_INVALID");
        }

        let result = recover_existing(&args[2], &args[3], &args[4], &args[5]).await;

        return match result {
            Ok(value) => {
                println!("{value}");
                println!("PHOENIX_EXECUTOR_ROTATION_RECOVERY_OK");
                ExitCode::SUCCESS
            }
            Err(code) => code,
        };
    }

    if !matches!(mode, "prepare" | "execute" | "rollback") || args.len() != 6 {
        return failure("ROTATION_ARGUMENTS_INVALID");
    }

    let plan = match load_plan(&args[2]) {
        Ok(value) => value,
        Err(code) => return code,
    };
    let creation = match validate_creation_bytecode(&plan, &args[3]) {
        Ok(value) => value,
        Err(_) => return failure("ROTATION_PLAN_INVALID"),
    };
    let endpoint_text = match required("LIVE_EXECUTOR_RPC_URL") {
        Ok(value) => value,
        Err(code) => return code,
    };
    let allowlist_text = match required("LIVE_EXECUTOR_RPC_ALLOWLIST") {
        Ok(value) => value,
        Err(code) => return code,
    };
    let endpoint = match Url::parse(&endpoint_text) {
        Ok(value) => value,
        Err(_) => return failure("ROTATION_RPC_INVALID"),
    };
    let allowlist = match allowlist_text
        .split(',')
        .map(|item| Url::parse(item.trim()))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(value) if value.len() == 1 && value[0] == endpoint => value,
        _ => return failure("ROTATION_RPC_INVALID"),
    };
    let header_name = match required("LIVE_EXECUTOR_RPC_HEADER_NAME") {
        Ok(value) => value,
        Err(code) => return code,
    };
    let header_file = match required("LIVE_EXECUTOR_RPC_HEADER_FILE") {
        Ok(value) => value,
        Err(code) => return code,
    };
    let rpc = match HttpExecutionRpc::new_production_authenticated(
        endpoint,
        &allowlist,
        &header_name,
        &header_file,
    ) {
        Ok(value) => value,
        Err(_) => return failure("ROTATION_RPC_INVALID"),
    };
    let signer_path = match required("PHOENIX_EXECUTOR_ROTATION_SIGNER_FILE") {
        Ok(value) => value,
        Err(code) => return code,
    };
    let signer = match transaction_signer_from_file(&signer_path, plan.chain_id) {
        Ok(value) => value,
        Err(_) => return failure("ROTATION_SIGNER_INVALID"),
    };
    let transport = match AuthenticatedPhoenixExecutorRotationTransport::new(
        &plan,
        rpc,
        signer,
        creation,
        PathBuf::from(&args[2]),
        PathBuf::from(&args[5]),
        PathBuf::from(&args[4]),
    ) {
        Ok(value) => value,
        Err(_) => return failure("ROTATION_CONTEXT_INVALID"),
    };
    if mode == "execute"
        && !transport.provenance().is_some_and(|value| {
            value.config_verified && value.pre_cutover_spl_absent && !value.rollback_used
        })
    {
        return failure("ROTATION_PREPARE_REQUIRED");
    }
    if mode == "rollback"
        && !transport.provenance().is_some_and(|value| {
            value.cutover_started && !value.rollback_used && !value.rollback_completed
        })
    {
        return failure("ROTATION_ROLLBACK_REJECTED");
    }
    let backend = match ProductionPhoenixExecutorRotationBackend::new(plan, transport) {
        Ok(value) => value,
        Err(_) => return failure("ROTATION_CONTEXT_INVALID"),
    };
    let mut operator = RotationOperator::new(backend);
    let result = match mode {
        "prepare" => operator.prepare().await,
        "execute" => operator.execute().await,
        "rollback" => operator.rollback_once().await,
        _ => unreachable!(),
    };
    if result.is_err() {
        return failure("PHOENIX_EXECUTOR_ROTATION_FAILED");
    }
    let backend = operator.into_backend();
    let transport = backend.into_transport();
    let Some(value) = transport.provenance() else {
        return failure("ROTATION_PROVENANCE_INVALID");
    };
    println!(
        "{}",
        json!({
            "config_tx_count": value.config_tx_hashes.len(),
            "cutover_completed": value.cutover_completed,
            "new_executor": value.new_executor,
            "rollback_used": value.rollback_used,
            "schema": value.schema,
            "status": mode
        })
    );
    ExitCode::SUCCESS
}
