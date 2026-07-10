use crate::config::EngineConfig;
use crate::execution::ExecutionMode;

const ARBITRUM_ONE_CHAIN_ID: u64 = 42161;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadinessState {
    ready: bool,
    detail: &'static str,
}

impl ReadinessState {
    pub fn initializing() -> Self {
        Self {
            ready: false,
            detail: "initializing",
        }
    }

    pub fn ready(detail: &'static str) -> Self {
        Self {
            ready: true,
            detail,
        }
    }

    pub fn not_ready(detail: &'static str) -> Self {
        Self {
            ready: false,
            detail,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }

    pub fn detail(&self) -> &'static str {
        self.detail
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeInit {
    pub mode: ExecutionMode,
    pub readiness_detail: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeInitError {
    detail: &'static str,
}

impl RuntimeInitError {
    pub fn detail(&self) -> &'static str {
        self.detail
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HealthResponse {
    pub status: u16,
    pub body: &'static str,
}

pub fn initialize_runtime(cfg: &EngineConfig) -> Result<RuntimeInit, RuntimeInitError> {
    validate_config(cfg)?;
    let mode = ExecutionMode::from_env(cfg.mode.as_str(), cfg.live_execution);
    let readiness_detail = match mode {
        ExecutionMode::Shadow => "shadow_runtime_ready",
        ExecutionMode::Simulate => "simulate_runtime_ready",
        ExecutionMode::Live => "live_runtime_initialized",
    };
    Ok(RuntimeInit {
        mode,
        readiness_detail,
    })
}

pub fn health_response(path: &str, readiness: &ReadinessState) -> Option<HealthResponse> {
    match path {
        "/healthz" => Some(HealthResponse {
            status: 200,
            body: "ok",
        }),
        "/readyz" if readiness.is_ready() => Some(HealthResponse {
            status: 200,
            body: readiness.detail(),
        }),
        "/readyz" => Some(HealthResponse {
            status: 503,
            body: readiness.detail(),
        }),
        _ => None,
    }
}

fn validate_config(cfg: &EngineConfig) -> Result<(), RuntimeInitError> {
    if !is_supported_mode(&cfg.mode) {
        return Err(invalid_config());
    }
    if cfg.chain_id != ARBITRUM_ONE_CHAIN_ID {
        return Err(invalid_config());
    }
    if !is_nats_url(&cfg.nats_url) {
        return Err(invalid_config());
    }
    Ok(())
}

fn is_supported_mode(mode: &str) -> bool {
    matches!(
        mode.to_ascii_uppercase().as_str(),
        "SHADOW" | "SIMULATE" | "LIVE"
    )
}

fn is_nats_url(value: &str) -> bool {
    let rest = match value.strip_prefix("nats://") {
        Some(rest) => rest,
        None => return false,
    };
    !rest.trim().is_empty()
}

fn invalid_config() -> RuntimeInitError {
    RuntimeInitError {
        detail: "invalid_runtime_configuration",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_endpoint_remains_live_when_process_is_alive() {
        let state = ReadinessState::initializing();
        assert_eq!(
            health_response("/healthz", &state),
            Some(HealthResponse {
                status: 200,
                body: "ok"
            })
        );
    }

    #[test]
    fn readiness_starts_false_before_runtime_initialization() {
        let state = ReadinessState::initializing();
        assert_eq!(
            health_response("/readyz", &state),
            Some(HealthResponse {
                status: 503,
                body: "initializing"
            })
        );
    }

    #[test]
    fn valid_shadow_initialization_transitions_readiness_to_ready() {
        let init = initialize_runtime(&shadow_config()).unwrap();
        let state = ReadinessState::ready(init.readiness_detail);
        assert_eq!(init.mode, ExecutionMode::Shadow);
        assert_eq!(
            health_response("/readyz", &state),
            Some(HealthResponse {
                status: 200,
                body: "shadow_runtime_ready"
            })
        );
    }

    #[test]
    fn invalid_initialization_leaves_readiness_false() {
        let cfg = EngineConfig {
            chain_id: 1,
            ..shadow_config()
        };
        let err = initialize_runtime(&cfg).unwrap_err();
        let state = ReadinessState::not_ready(err.detail());
        assert_eq!(
            health_response("/readyz", &state),
            Some(HealthResponse {
                status: 503,
                body: "invalid_runtime_configuration"
            })
        );
    }

    #[test]
    fn shadow_readiness_does_not_require_executor_address() {
        assert_eq!(initialize_runtime(&shadow_config()).unwrap().mode, ExecutionMode::Shadow);
    }

    #[test]
    fn shadow_readiness_does_not_require_signer_private_key() {
        assert_eq!(initialize_runtime(&shadow_config()).unwrap().mode, ExecutionMode::Shadow);
    }

    #[test]
    fn shadow_readiness_does_not_require_profitability_evidence() {
        let init = initialize_runtime(&shadow_config()).unwrap();
        assert_eq!(init.readiness_detail, "shadow_runtime_ready");
    }

    #[test]
    fn live_without_execution_flag_does_not_enable_live_execution() {
        let cfg = EngineConfig {
            mode: "LIVE".to_string(),
            live_execution: false,
            ..shadow_config()
        };
        assert_eq!(initialize_runtime(&cfg).unwrap().mode, ExecutionMode::Shadow);
    }

    #[test]
    fn explicit_live_mode_derives_live_execution_without_release_claim() {
        let cfg = EngineConfig {
            mode: "LIVE".to_string(),
            live_execution: true,
            ..shadow_config()
        };
        let init = initialize_runtime(&cfg).unwrap();
        assert_eq!(init.mode, ExecutionMode::Live);
        assert_eq!(init.readiness_detail, "live_runtime_initialized");
    }

    #[test]
    fn readiness_detail_does_not_expose_secret_bearing_environment_values() {
        let cfg = EngineConfig {
            nats_url: "nats://user:secret-token@nats:4222".to_string(),
            chain_id: 1,
            ..shadow_config()
        };
        let err = initialize_runtime(&cfg).unwrap_err();
        assert_eq!(err.detail(), "invalid_runtime_configuration");
        assert!(!err.detail().contains("secret-token"));
        assert!(!err.detail().contains("user:"));
    }

    fn shadow_config() -> EngineConfig {
        EngineConfig {
            mode: "SHADOW".to_string(),
            live_execution: false,
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            nats_url: "nats://nats:4222".to_string(),
        }
    }
}
