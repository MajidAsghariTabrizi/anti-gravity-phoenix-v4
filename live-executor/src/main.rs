use phoenix_live_executor::config::{Bootstrap, ConfigError, DisabledReason};
use phoenix_live_executor::engine::LiveExecutor;
use phoenix_live_executor::rpc::HttpExecutionRpc;
use phoenix_live_executor::store::{ExecutorStore, PostgresExecutorStore};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let bootstrap = match Bootstrap::from_environment() {
        Ok(bootstrap) => bootstrap,
        Err(error) => {
            error!(
                error_code = config_error_code(&error),
                "live executor refused to start"
            );
            std::process::exit(1);
        }
    };
    let Bootstrap::Armed(armed) = bootstrap else {
        let Bootstrap::Disabled(reason) = bootstrap else {
            unreachable!("bootstrap variants are exhaustive");
        };
        info!(
            state = "disabled",
            reason = disabled_reason_code(reason),
            "live executor is disabled"
        );
        return;
    };
    let config = armed.config;
    let signer = armed.signer;

    let store = match PostgresExecutorStore::connect(&config.postgres_dsn).await {
        Ok(store) => store,
        Err(_) => {
            error!(
                error_code = "database_connection",
                "live executor failed closed"
            );
            std::process::exit(1);
        }
    };
    if store.validate_schema().await.is_err() {
        error!(
            error_code = "schema_contract",
            "live executor failed closed"
        );
        std::process::exit(1);
    }
    let rpc = match HttpExecutionRpc::new_production(config.rpc_url.clone(), &config.rpc_allowlist)
    {
        Ok(rpc) => rpc,
        Err(_) => {
            let _ = store.disarm("rpc_allowlist_failure").await;
            error!(error_code = "rpc_allowlist", "live executor failed closed");
            std::process::exit(1);
        }
    };
    let executor = LiveExecutor::new(config, signer, store.clone(), rpc);
    info!(
        state = "started_disarmed_until_db_gate",
        "live executor started"
    );

    loop {
        match executor.step(chrono::Utc::now()).await {
            Ok(state) => info!(state = state.code(), "live executor state transition"),
            Err(_) => {
                let _ = store.disarm("executor_runtime_failure").await;
                error!(
                    error_code = "executor_runtime",
                    "live executor failed closed"
                );
                std::process::exit(1);
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(executor.poll_interval()) => {}
            signal = tokio::signal::ctrl_c() => {
                if signal.is_err() {
                    error!(error_code = "signal_handler", "live executor failed closed");
                    std::process::exit(1);
                }
                info!(state = "stopped", "live executor stopped");
                return;
            }
        }
    }
}

fn disabled_reason_code(reason: DisabledReason) -> &'static str {
    match reason {
        DisabledReason::SafeDefaults => "safe_defaults",
        DisabledReason::EnvironmentKillSwitch => "environment_kill_switch",
    }
}

fn config_error_code(error: &ConfigError) -> &'static str {
    match error {
        ConfigError::IncompleteArming => "incomplete_arming",
        ConfigError::Missing(_) => "missing_setting",
        ConfigError::InvalidBoolean => "invalid_boolean",
        ConfigError::InvalidChain => "invalid_chain",
        ConfigError::InvalidAddress => "invalid_address",
        ConfigError::InvalidCodeHash => "invalid_code_hash",
        ConfigError::UnsupportedProfitAsset => "unsupported_profit_asset",
        ConfigError::Signer(_) => "invalid_signer",
        ConfigError::WalletMismatch => "wallet_mismatch",
        ConfigError::InvalidRpcUrl => "invalid_rpc_url",
        ConfigError::RpcNotAllowlisted => "rpc_not_allowlisted",
        ConfigError::InvalidLimit => "invalid_limit",
        ConfigError::ConcurrentCanaryForbidden => "concurrent_canary_forbidden",
    }
}
