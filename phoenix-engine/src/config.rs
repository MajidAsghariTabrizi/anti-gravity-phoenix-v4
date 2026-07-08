use std::env;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineConfig {
    pub mode: String,
    pub live_execution: bool,
    pub chain_id: u64,
    pub nats_url: String,
}

impl EngineConfig {
    pub fn from_env() -> Self {
        Self {
            mode: env::var("PHOENIX_MODE").unwrap_or_else(|_| "SHADOW".to_string()),
            live_execution: env::var("LIVE_EXECUTION")
                .map(|v| v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            chain_id: env::var("CHAIN_ID")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(42161),
            nats_url: env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string()),
        }
    }
}

