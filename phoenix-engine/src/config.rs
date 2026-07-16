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
            chain_id: parse_chain_id(env::var("CHAIN_ID").ok()),
            nats_url: env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string()),
        }
    }
}

fn parse_chain_id(value: Option<String>) -> u64 {
    match value {
        Some(value) => value.parse().unwrap_or(0),
        None => 42161,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_chain_id;

    #[test]
    fn missing_chain_id_defaults_to_arbitrum_one() {
        assert_eq!(parse_chain_id(None), 42161);
    }

    #[test]
    fn malformed_chain_id_is_not_silently_defaulted() {
        assert_eq!(parse_chain_id(Some("not-a-chain".to_string())), 0);
    }
}
