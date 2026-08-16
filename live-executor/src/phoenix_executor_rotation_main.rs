use phoenix_live_executor::config::transaction_signer_from_file;
use phoenix_live_executor::executor_rotation::{
    persist_provenance, read_provenance, validate_creation_bytecode, validate_plan,
    AuthenticatedPhoenixExecutorRotationTransport, ProductionPhoenixExecutorRotationBackend,
    RotationOperator, RotationPlan, OLD_EXECUTOR,
};
use phoenix_live_executor::rpc::HttpExecutionRpc;
use phoenix_live_executor::store::PostgresExecutorStore;
use serde_json::json;
use std::{env, fs, path::PathBuf, process::ExitCode};
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
