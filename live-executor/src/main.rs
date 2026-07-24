use phoenix_live_executor::autonomous::{
    AutonomousMaterializer, AutonomousMaterializerError, MaterializationState,
};
use phoenix_live_executor::config::{Bootstrap, DisabledReason};
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
            error!(error_code = error.code(), "live executor refused to start");
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
    let materializer = match AutonomousMaterializer::connect(config.clone(), rpc.clone()).await {
        Ok(materializer) => materializer,
        Err(_) => {
            let _ = store.disarm("autonomous_materializer_startup").await;
            error!(
                error_code = "autonomous_materializer_startup",
                "live executor failed closed"
            );
            std::process::exit(1);
        }
    };
    let executor = LiveExecutor::new(config, signer, store.clone(), rpc);
    info!(
        state = "started_disarmed_until_db_gate",
        "live executor started"
    );

    loop {
        let now = chrono::Utc::now();
        match materializer.step(now).await {
            Ok(MaterializationState::Idle) => {}
            Ok(MaterializationState::Materialized { .. }) => {
                info!(
                    state = "request_materialized",
                    "autonomous request materialized"
                );
            }
            Ok(MaterializationState::Rejected { reason, .. }) => {
                info!(
                    state = "candidate_rejected",
                    reason, "autonomous candidate rejected"
                );
            }
            Err(AutonomousMaterializerError::Dependency) => {
                error!(
                    error_code = "autonomous_dependency",
                    "autonomous materializer dependency unavailable"
                );
            }
            Err(AutonomousMaterializerError::Policy) => {
                info!(
                    state = "candidate_rejected",
                    reason = "policy",
                    "autonomous candidate rejected"
                );
            }
            Err(_) => {
                let _ = store.disarm("autonomous_materializer_integrity").await;
                error!(
                    error_code = "autonomous_materializer_integrity",
                    "live executor failed closed"
                );
                std::process::exit(1);
            }
        }
        match executor.step(now).await {
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
