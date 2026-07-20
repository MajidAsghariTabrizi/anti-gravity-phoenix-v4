use crate::model::{canonical_digest, CanonicalAddress};
use crate::signer::{SignerError, TransactionSigner};
use crate::{ARBITRUM_ONE_CHAIN_ID, ARBITRUM_WETH_ADDRESS};
use std::collections::BTreeMap;
use std::time::Duration;
use thiserror::Error;
use url::Url;
use zeroize::Zeroizing;

const MAX_RECEIPT_TIMEOUT_SECONDS: u64 = 600;
const MAX_POLL_INTERVAL_SECONDS: u64 = 30;
const ENVIRONMENT_NAMES: &[&str] = &[
    "PHOENIX_MODE",
    "LIVE_EXECUTION",
    "LIVE_EXECUTOR_ARMED",
    "LIVE_EXECUTOR_KILL_SWITCH",
    "CHAIN_ID",
    "WALLET_ADDRESS",
    "EXECUTOR_ADDRESS",
    "LIVE_EXECUTOR_EXECUTOR_CODE_HASH",
    "LIVE_EXECUTOR_PNL_ASSET_ADDRESS",
    "SIGNER_PRIVATE_KEY",
    "LIVE_EXECUTOR_RPC_URL",
    "LIVE_EXECUTOR_RPC_ALLOWLIST",
    "LIVE_EXECUTOR_MAX_GAS_LIMIT",
    "LIVE_EXECUTOR_MAX_MAX_FEE_PER_GAS_WEI",
    "LIVE_EXECUTOR_MAX_PRIORITY_FEE_PER_GAS_WEI",
    "LIVE_EXECUTOR_MAX_INPUT_AMOUNT",
    "LIVE_EXECUTOR_MIN_EXPECTED_PROFIT",
    "LIVE_EXECUTOR_MAX_DAILY_LOSS_WEI",
    "LIVE_EXECUTOR_RECEIPT_TIMEOUT_SECONDS",
    "LIVE_EXECUTOR_POLL_INTERVAL_SECONDS",
    "LIVE_EXECUTOR_ONE_TRANSACTION_AT_A_TIME",
    "POSTGRES_DSN",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisabledReason {
    SafeDefaults,
    EnvironmentKillSwitch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafetyLimits {
    pub maximum_gas_limit: u64,
    pub maximum_max_fee_per_gas: u128,
    pub maximum_priority_fee_per_gas: u128,
    pub maximum_input_amount: u128,
    pub minimum_expected_profit: u128,
    pub maximum_daily_loss_wei: u128,
}

#[derive(Clone)]
pub struct ExecutorConfig {
    pub postgres_dsn: String,
    pub rpc_url: Url,
    pub rpc_allowlist: Vec<Url>,
    pub wallet_address: CanonicalAddress,
    pub executor_address: CanonicalAddress,
    pub executor_code_hash: String,
    pub pnl_asset_address: CanonicalAddress,
    pub chain_id: u64,
    pub limits: SafetyLimits,
    pub receipt_timeout: Duration,
    pub poll_interval: Duration,
    pub one_transaction_at_a_time: bool,
}

pub enum Bootstrap {
    Disabled(DisabledReason),
    Armed(Box<ArmedBootstrap>),
}

pub struct ArmedBootstrap {
    pub config: ExecutorConfig,
    pub signer: TransactionSigner,
}

impl Bootstrap {
    pub fn from_environment() -> Result<Self, ConfigError> {
        let values = ENVIRONMENT_NAMES
            .iter()
            .filter_map(|name| {
                std::env::var(name)
                    .ok()
                    .map(|value| ((*name).to_string(), value))
            })
            .collect::<BTreeMap<_, _>>();
        Self::from_values(values)
    }

    pub fn from_values(mut values: BTreeMap<String, String>) -> Result<Self, ConfigError> {
        let key_material = values.remove("SIGNER_PRIVATE_KEY").map(Zeroizing::new);
        let mode = get_or(&values, "PHOENIX_MODE", "SHADOW");
        let live_execution = parse_bool(get_or(&values, "LIVE_EXECUTION", "false"))?;
        let armed = parse_bool(get_or(&values, "LIVE_EXECUTOR_ARMED", "false"))?;

        if mode == "SHADOW" && !live_execution && !armed {
            return Ok(Self::Disabled(DisabledReason::SafeDefaults));
        }
        if mode != "LIVE" || !live_execution || !armed {
            return Err(ConfigError::IncompleteArming);
        }
        if parse_bool(get_or(&values, "LIVE_EXECUTOR_KILL_SWITCH", "true"))? {
            return Ok(Self::Disabled(DisabledReason::EnvironmentKillSwitch));
        }

        let chain_id = required(&values, "CHAIN_ID")?
            .parse::<u64>()
            .map_err(|_| ConfigError::InvalidChain)?;
        if chain_id != ARBITRUM_ONE_CHAIN_ID {
            return Err(ConfigError::InvalidChain);
        }
        let wallet_address = CanonicalAddress::parse(required(&values, "WALLET_ADDRESS")?)
            .map_err(|_| ConfigError::InvalidAddress)?;
        let executor_address = CanonicalAddress::parse(required(&values, "EXECUTOR_ADDRESS")?)
            .map_err(|_| ConfigError::InvalidAddress)?;
        let executor_code_hash = required(&values, "LIVE_EXECUTOR_EXECUTOR_CODE_HASH")?.to_string();
        if !canonical_digest(&executor_code_hash) {
            return Err(ConfigError::InvalidCodeHash);
        }
        let pnl_asset_address =
            CanonicalAddress::parse(required(&values, "LIVE_EXECUTOR_PNL_ASSET_ADDRESS")?)
                .map_err(|_| ConfigError::InvalidAddress)?;
        if pnl_asset_address
            != CanonicalAddress::parse(ARBITRUM_WETH_ADDRESS)
                .map_err(|_| ConfigError::InvalidAddress)?
        {
            return Err(ConfigError::UnsupportedProfitAsset);
        }

        let key_material = key_material.ok_or(ConfigError::Missing("SIGNER_PRIVATE_KEY"))?;
        let signer_result = TransactionSigner::from_secret(&key_material, chain_id);
        let signer = signer_result.map_err(ConfigError::Signer)?;
        if signer.address() != wallet_address {
            return Err(ConfigError::WalletMismatch);
        }

        let rpc_url = parse_url(required(&values, "LIVE_EXECUTOR_RPC_URL")?)?;
        let rpc_allowlist = required(&values, "LIVE_EXECUTOR_RPC_ALLOWLIST")?
            .split(',')
            .map(parse_url)
            .collect::<Result<Vec<_>, _>>()?;
        if rpc_allowlist.is_empty() || !rpc_allowlist.iter().any(|allowed| allowed == &rpc_url) {
            return Err(ConfigError::RpcNotAllowlisted);
        }
        if rpc_url.scheme() != "https"
            || rpc_url.host_str().is_none()
            || rpc_url.fragment().is_some()
            || !rpc_url.username().is_empty()
            || rpc_url.password().is_some()
        {
            return Err(ConfigError::InvalidRpcUrl);
        }

        let maximum_gas_limit = positive_u64(&values, "LIVE_EXECUTOR_MAX_GAS_LIMIT")?;
        let maximum_max_fee_per_gas =
            positive_u128(&values, "LIVE_EXECUTOR_MAX_MAX_FEE_PER_GAS_WEI")?;
        let maximum_priority_fee_per_gas =
            positive_u128(&values, "LIVE_EXECUTOR_MAX_PRIORITY_FEE_PER_GAS_WEI")?;
        if maximum_priority_fee_per_gas > maximum_max_fee_per_gas {
            return Err(ConfigError::InvalidLimit);
        }
        let limits = SafetyLimits {
            maximum_gas_limit,
            maximum_max_fee_per_gas,
            maximum_priority_fee_per_gas,
            maximum_input_amount: positive_u128(&values, "LIVE_EXECUTOR_MAX_INPUT_AMOUNT")?,
            minimum_expected_profit: positive_u128(&values, "LIVE_EXECUTOR_MIN_EXPECTED_PROFIT")?,
            maximum_daily_loss_wei: positive_u128(&values, "LIVE_EXECUTOR_MAX_DAILY_LOSS_WEI")?,
        };
        let receipt_timeout_seconds =
            positive_u64(&values, "LIVE_EXECUTOR_RECEIPT_TIMEOUT_SECONDS")?;
        if receipt_timeout_seconds > MAX_RECEIPT_TIMEOUT_SECONDS {
            return Err(ConfigError::InvalidLimit);
        }
        let poll_interval_seconds = positive_u64(&values, "LIVE_EXECUTOR_POLL_INTERVAL_SECONDS")?;
        if poll_interval_seconds > MAX_POLL_INTERVAL_SECONDS {
            return Err(ConfigError::InvalidLimit);
        }
        let one_transaction_at_a_time = parse_bool(required(
            &values,
            "LIVE_EXECUTOR_ONE_TRANSACTION_AT_A_TIME",
        )?)?;
        if !one_transaction_at_a_time {
            return Err(ConfigError::ConcurrentCanaryForbidden);
        }
        let postgres_dsn = required(&values, "POSTGRES_DSN")?.to_string();
        if postgres_dsn.trim().is_empty() {
            return Err(ConfigError::Missing("POSTGRES_DSN"));
        }

        Ok(Self::Armed(Box::new(ArmedBootstrap {
            config: ExecutorConfig {
                postgres_dsn,
                rpc_url,
                rpc_allowlist,
                wallet_address,
                executor_address,
                executor_code_hash,
                pnl_asset_address,
                chain_id,
                limits,
                receipt_timeout: Duration::from_secs(receipt_timeout_seconds),
                poll_interval: Duration::from_secs(poll_interval_seconds),
                one_transaction_at_a_time,
            },
            signer,
        })))
    }
}

fn required<'a>(
    values: &'a BTreeMap<String, String>,
    name: &'static str,
) -> Result<&'a str, ConfigError> {
    values
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or(ConfigError::Missing(name))
}

fn get_or<'a>(values: &'a BTreeMap<String, String>, name: &str, fallback: &'a str) -> &'a str {
    values.get(name).map(String::as_str).unwrap_or(fallback)
}

fn parse_bool(value: &str) -> Result<bool, ConfigError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(ConfigError::InvalidBoolean),
    }
}

fn positive_u64(values: &BTreeMap<String, String>, name: &'static str) -> Result<u64, ConfigError> {
    required(values, name)?
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or(ConfigError::InvalidLimit)
}

fn positive_u128(
    values: &BTreeMap<String, String>,
    name: &'static str,
) -> Result<u128, ConfigError> {
    required(values, name)?
        .parse::<u128>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or(ConfigError::InvalidLimit)
}

fn parse_url(value: &str) -> Result<Url, ConfigError> {
    Url::parse(value).map_err(|_| ConfigError::InvalidRpcUrl)
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("live executor arming is incomplete")]
    IncompleteArming,
    #[error("missing required live executor setting")]
    Missing(&'static str),
    #[error("invalid boolean setting")]
    InvalidBoolean,
    #[error("invalid chain")]
    InvalidChain,
    #[error("invalid address")]
    InvalidAddress,
    #[error("executor code hash is invalid")]
    InvalidCodeHash,
    #[error("live canary profit asset must be Arbitrum WETH")]
    UnsupportedProfitAsset,
    #[error("signer configuration is invalid")]
    Signer(#[source] SignerError),
    #[error("signer and wallet do not match")]
    WalletMismatch,
    #[error("RPC URL is invalid")]
    InvalidRpcUrl,
    #[error("RPC URL is not allowlisted")]
    RpcNotAllowlisted,
    #[error("safety limit is invalid")]
    InvalidLimit,
    #[error("concurrent canary execution is forbidden")]
    ConcurrentCanaryForbidden,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_signer_local::PrivateKeySigner;
    use std::str::FromStr;

    fn fully_armed_values() -> BTreeMap<String, String> {
        let private_key = hex::encode([7_u8; 32]);
        let signer = PrivateKeySigner::from_str(&private_key).expect("test signer");
        BTreeMap::from([
            ("PHOENIX_MODE".to_string(), "LIVE".to_string()),
            ("LIVE_EXECUTION".to_string(), "true".to_string()),
            ("LIVE_EXECUTOR_ARMED".to_string(), "true".to_string()),
            ("LIVE_EXECUTOR_KILL_SWITCH".to_string(), "false".to_string()),
            ("CHAIN_ID".to_string(), ARBITRUM_ONE_CHAIN_ID.to_string()),
            (
                "WALLET_ADDRESS".to_string(),
                signer.address().to_string().to_lowercase(),
            ),
            (
                "EXECUTOR_ADDRESS".to_string(),
                "0x1111111111111111111111111111111111111111".to_string(),
            ),
            (
                "LIVE_EXECUTOR_EXECUTOR_CODE_HASH".to_string(),
                "a".repeat(64),
            ),
            (
                "LIVE_EXECUTOR_PNL_ASSET_ADDRESS".to_string(),
                ARBITRUM_WETH_ADDRESS.to_string(),
            ),
            ("SIGNER_PRIVATE_KEY".to_string(), private_key),
            (
                "LIVE_EXECUTOR_RPC_URL".to_string(),
                "https://rpc.example.invalid/path".to_string(),
            ),
            (
                "LIVE_EXECUTOR_RPC_ALLOWLIST".to_string(),
                "https://rpc.example.invalid/path".to_string(),
            ),
            (
                "LIVE_EXECUTOR_MAX_GAS_LIMIT".to_string(),
                "500000".to_string(),
            ),
            (
                "LIVE_EXECUTOR_MAX_MAX_FEE_PER_GAS_WEI".to_string(),
                "1000000000".to_string(),
            ),
            (
                "LIVE_EXECUTOR_MAX_PRIORITY_FEE_PER_GAS_WEI".to_string(),
                "100000000".to_string(),
            ),
            (
                "LIVE_EXECUTOR_MAX_INPUT_AMOUNT".to_string(),
                "1000000000000000".to_string(),
            ),
            (
                "LIVE_EXECUTOR_MIN_EXPECTED_PROFIT".to_string(),
                "1000000000000".to_string(),
            ),
            (
                "LIVE_EXECUTOR_MAX_DAILY_LOSS_WEI".to_string(),
                "100000000000000".to_string(),
            ),
            (
                "LIVE_EXECUTOR_RECEIPT_TIMEOUT_SECONDS".to_string(),
                "90".to_string(),
            ),
            (
                "LIVE_EXECUTOR_POLL_INTERVAL_SECONDS".to_string(),
                "1".to_string(),
            ),
            (
                "LIVE_EXECUTOR_ONE_TRANSACTION_AT_A_TIME".to_string(),
                "true".to_string(),
            ),
            (
                "POSTGRES_DSN".to_string(),
                "postgres://localhost/phoenix".to_string(),
            ),
        ])
    }

    #[test]
    fn safe_defaults_are_disabled_without_secrets() {
        let result = Bootstrap::from_values(BTreeMap::new()).expect("safe default");
        assert!(matches!(
            result,
            Bootstrap::Disabled(DisabledReason::SafeDefaults)
        ));
    }

    #[test]
    fn shadow_cannot_be_armed() {
        let mut values = fully_armed_values();
        values.insert("PHOENIX_MODE".to_string(), "SHADOW".to_string());
        assert!(matches!(
            Bootstrap::from_values(values),
            Err(ConfigError::IncompleteArming)
        ));
    }

    #[test]
    fn incomplete_arming_cannot_submit() {
        let mut values = fully_armed_values();
        values.insert("LIVE_EXECUTION".to_string(), "false".to_string());
        assert!(matches!(
            Bootstrap::from_values(values),
            Err(ConfigError::IncompleteArming)
        ));
    }

    #[test]
    fn environment_kill_switch_disables_before_signer_loading() {
        let mut values = fully_armed_values();
        values.insert("LIVE_EXECUTOR_KILL_SWITCH".to_string(), "true".to_string());
        values.insert(
            "SIGNER_PRIVATE_KEY".to_string(),
            "not-a-private-key".to_string(),
        );
        assert!(matches!(
            Bootstrap::from_values(values),
            Ok(Bootstrap::Disabled(DisabledReason::EnvironmentKillSwitch))
        ));
    }

    #[test]
    fn wallet_must_match_signer() {
        let mut values = fully_armed_values();
        values.insert(
            "WALLET_ADDRESS".to_string(),
            "0x3333333333333333333333333333333333333333".to_string(),
        );
        assert!(matches!(
            Bootstrap::from_values(values),
            Err(ConfigError::WalletMismatch)
        ));
    }

    #[test]
    fn executor_address_must_be_nonzero_and_canonical() {
        let mut values = fully_armed_values();
        values.insert(
            "EXECUTOR_ADDRESS".to_string(),
            "0x0000000000000000000000000000000000000000".to_string(),
        );
        assert!(matches!(
            Bootstrap::from_values(values),
            Err(ConfigError::InvalidAddress)
        ));
    }

    #[test]
    fn executor_code_hash_must_be_canonical() {
        let mut values = fully_armed_values();
        values.insert(
            "LIVE_EXECUTOR_EXECUTOR_CODE_HASH".to_string(),
            "0xnot-a-canonical-digest".to_string(),
        );
        assert!(matches!(
            Bootstrap::from_values(values),
            Err(ConfigError::InvalidCodeHash)
        ));
    }

    #[test]
    fn chain_must_be_arbitrum_one() {
        let mut values = fully_armed_values();
        values.insert("CHAIN_ID".to_string(), "1".to_string());
        assert!(matches!(
            Bootstrap::from_values(values),
            Err(ConfigError::InvalidChain)
        ));
    }

    #[test]
    fn selected_rpc_must_be_exactly_allowlisted() {
        let mut values = fully_armed_values();
        values.insert(
            "LIVE_EXECUTOR_RPC_ALLOWLIST".to_string(),
            "https://other.example.invalid/path".to_string(),
        );
        assert!(matches!(
            Bootstrap::from_values(values),
            Err(ConfigError::RpcNotAllowlisted)
        ));
    }

    #[test]
    fn non_weth_profit_asset_is_rejected() {
        let mut values = fully_armed_values();
        values.insert(
            "LIVE_EXECUTOR_PNL_ASSET_ADDRESS".to_string(),
            "0x2222222222222222222222222222222222222222".to_string(),
        );
        assert!(matches!(
            Bootstrap::from_values(values),
            Err(ConfigError::UnsupportedProfitAsset)
        ));
    }

    #[test]
    fn one_transaction_mode_is_mandatory() {
        let mut values = fully_armed_values();
        values.insert(
            "LIVE_EXECUTOR_ONE_TRANSACTION_AT_A_TIME".to_string(),
            "false".to_string(),
        );
        assert!(matches!(
            Bootstrap::from_values(values),
            Err(ConfigError::ConcurrentCanaryForbidden)
        ));
    }
}
