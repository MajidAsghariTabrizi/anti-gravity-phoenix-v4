use chrono::Utc;
use phoenix_fork_sandbox::{
    ForkEvidenceStore, ForkRunner, HttpForkRpc, PlanPolicy, UnsignedPlanner,
};
use serde_json::json;
use std::collections::BTreeSet;
use std::env;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
enum MainError {
    #[error("fork sandbox configuration is invalid")]
    Configuration,
    #[error("fork sandbox command is invalid")]
    Command,
    #[error("fork sandbox operation failed: {0}")]
    Operation(String),
}

struct Config {
    database_url: String,
    database_ssl_mode: String,
    fork_rpc_url: String,
    policy: PlanPolicy,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), MainError> {
    let decision_id = parse_command()?;
    let config = Config::from_env()?;
    let store = ForkEvidenceStore::connect(&config.database_url, &config.database_ssl_mode)
        .await
        .map_err(operation)?;
    let fact = store
        .load_opportunity(&decision_id)
        .await
        .map_err(operation)?;
    let now = Utc::now();
    let now_unix_ms =
        u64::try_from(now.timestamp_millis()).map_err(|_| MainError::Configuration)?;
    let plan = UnsignedPlanner
        .build(&fact, &config.policy, now_unix_ms)
        .map_err(operation)?;
    let rpc = HttpForkRpc::new(&config.fork_rpc_url, Duration::from_secs(10)).map_err(operation)?;
    let result = ForkRunner.run(&plan, &rpc, now).await.map_err(operation)?;
    store
        .persist_result(&plan, &result)
        .await
        .map_err(operation)?;
    let output = serde_json::to_string_pretty(&json!({
        "plan_hash": result.body.plan_hash,
        "plan": plan,
        "result": result,
    }))
    .map_err(|_| MainError::Operation("evidence serialization failed".to_string()))?;
    println!("{output}");
    Ok(())
}

impl Config {
    fn from_env() -> Result<Self, MainError> {
        if env::var("PHOENIX_FORK_MODE").as_deref() != Ok("isolated-anvil")
            || env::var("PHOENIX_MODE").as_deref() != Ok("SHADOW")
            || env::var("LIVE_EXECUTION").as_deref() != Ok("false")
            || unsafe_environment_present()
        {
            return Err(MainError::Configuration);
        }
        let policy = PlanPolicy {
            allowed_tokens: address_set("FORK_ALLOWED_TOKENS")?,
            allowed_pools: address_set("FORK_ALLOWED_POOLS")?,
            allowed_routers: address_set("FORK_ALLOWED_ROUTERS")?,
            allowed_protocols: string_set("FORK_ALLOWED_PROTOCOLS")?,
            target_contract: canonical_env("FORK_TARGET_CONTRACT")?,
            target_code_hash: canonical_env("FORK_TARGET_CODE_HASH")?,
            simulation_from: canonical_env("FORK_SIMULATION_FROM")?,
            minimum_net_pnl: parse_env("FORK_MINIMUM_NET_PNL")?,
            maximum_input_amount: parse_env("FORK_MAXIMUM_INPUT_AMOUNT")?,
            slippage_bps: parse_env("FORK_SLIPPAGE_BPS")?,
            maximum_calldata_bytes: env::var("FORK_MAXIMUM_CALLDATA_BYTES")
                .ok()
                .map(|value| value.parse().map_err(|_| MainError::Configuration))
                .transpose()?
                .unwrap_or(65_536),
        };
        Ok(Self {
            database_url: required_env("DATABASE_URL")?,
            database_ssl_mode: env::var("DATABASE_SSL_MODE")
                .unwrap_or_else(|_| "prefer".to_string()),
            fork_rpc_url: required_env("FORK_RPC_URL")?,
            policy,
        })
    }
}

fn parse_command() -> Result<String, MainError> {
    let mut args = env::args().skip(1);
    if args.next().as_deref() != Some("run") || args.next().as_deref() != Some("--decision-id") {
        return Err(MainError::Command);
    }
    let decision_id = args.next().ok_or(MainError::Command)?;
    if args.next().is_some() {
        return Err(MainError::Command);
    }
    Ok(decision_id)
}

fn unsafe_environment_present() -> bool {
    [
        "SIGNER_PRIVATE_KEY",
        "PRIVATE_KEY",
        "MNEMONIC",
        "WALLET_ADDRESS",
        "EXECUTOR_ADDRESS",
    ]
    .iter()
    .any(|name| env::var(name).is_ok_and(|value| !value.trim().is_empty()))
}

fn required_env(name: &str) -> Result<String, MainError> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty() && value.len() <= 4096)
        .ok_or(MainError::Configuration)
}

fn canonical_env(name: &str) -> Result<String, MainError> {
    Ok(required_env(name)?.to_ascii_lowercase())
}

fn address_set(name: &str) -> Result<BTreeSet<String>, MainError> {
    Ok(string_set(name)?
        .into_iter()
        .map(|value| value.to_ascii_lowercase())
        .collect())
}

fn string_set(name: &str) -> Result<BTreeSet<String>, MainError> {
    let values = required_env(name)?
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    if values.is_empty() || values.len() > 64 {
        Err(MainError::Configuration)
    } else {
        Ok(values)
    }
}

fn parse_env<T>(name: &str) -> Result<T, MainError>
where
    T: std::str::FromStr,
{
    required_env(name)?
        .parse()
        .map_err(|_| MainError::Configuration)
}

fn operation(error: impl std::fmt::Display) -> MainError {
    MainError::Operation(error.to_string())
}
