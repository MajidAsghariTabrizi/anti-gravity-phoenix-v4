use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use thiserror::Error;

pub const SHADOW_STATE_SCHEMA_VERSION: &str = "phoenix.rpc.shadow_state.v4";
pub const ARBITRUM_ONE_CHAIN_ID: u64 = 42161;
pub const MAX_POOLS_PER_REQUEST: usize = 16;
pub const MAX_GATEWAY_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_GATEWAY_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ShadowStateRequest {
    pub schema_version: String,
    pub chain_id: u64,
    pub route_fingerprint: String,
    pub pools: Vec<PoolStateRequest>,
    pub evidence: EvidenceRequest,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "stage", rename_all = "snake_case", deny_unknown_fields)]
pub enum EvidenceRequest {
    Primary,
    Verify {
        block_number: u64,
        block_hash: String,
        primary_state_hash: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PoolStateRequest {
    pub pool_id: String,
    pub address: String,
    pub protocol: String,
    pub token0: String,
    pub token1: String,
    pub token0_decimals: u8,
    pub token1_decimals: u8,
    pub fee: u32,
    pub tick_spacing: i32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ShadowStateResponse {
    pub schema_version: String,
    pub chain_id: u64,
    pub request_hash: String,
    pub route_config_hash: String,
    pub block_number: u64,
    pub block_hash: String,
    pub state_hash: String,
    pub pools: Vec<PoolStateResponse>,
    pub primary_provider_id: String,
    pub agreement_provider_id: Option<String>,
    pub secondary_state_hash: Option<String>,
    pub secondary_block_number: Option<u64>,
    pub secondary_block_hash: Option<String>,
    pub secondary_route_config_hash: Option<String>,
    pub provider_agreement: bool,
    pub verification_status: VerificationStatus,
    pub independent_verification_status: IndependentVerificationStatus,
    pub quality: Vec<RpcQualityEvidence>,
    pub resolved_at_unix_ms: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    PrimaryOnly,
    Agreed,
    Disagreed,
    SecondaryUnavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IndependentVerificationStatus {
    NotRequested,
    Requested,
    Agreed,
    Disagreed,
    ProviderUnavailable,
    IntegrityFailure,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PoolStateResponse {
    pub pool_id: String,
    pub address: String,
    pub protocol: String,
    pub token0: String,
    pub token1: String,
    pub token0_decimals: u8,
    pub token1_decimals: u8,
    pub fee: u32,
    pub tick_spacing: i32,
    pub slot0: String,
    pub liquidity: String,
    pub state_hash: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RpcQualityEvidence {
    pub provider_id: String,
    pub method: String,
    pub block_number: Option<u64>,
    pub block_hash: Option<String>,
    pub response_hash: Option<String>,
    pub latency_ns: u64,
    pub success: bool,
    pub stale_result: bool,
    pub disagreement: bool,
    pub timeout: bool,
    pub retry_count: u16,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GatewayErrorResponse {
    pub schema_version: String,
    pub error_class: String,
    pub retryable: bool,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ContractError {
    #[error("SHADOW state request schema is unsupported")]
    UnsupportedSchema,
    #[error("SHADOW state request chain is unsupported")]
    UnsupportedChain,
    #[error("SHADOW state request route identity is invalid")]
    InvalidRoute,
    #[error("SHADOW state request pool set is invalid")]
    InvalidPools,
    #[error("SHADOW state request evidence stage is invalid")]
    InvalidEvidence,
}

impl ShadowStateRequest {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.schema_version != SHADOW_STATE_SCHEMA_VERSION {
            return Err(ContractError::UnsupportedSchema);
        }
        if self.chain_id != ARBITRUM_ONE_CHAIN_ID {
            return Err(ContractError::UnsupportedChain);
        }
        if !bounded(&self.route_fingerprint, 1, 256) {
            return Err(ContractError::InvalidRoute);
        }
        if self.pools.is_empty() || self.pools.len() > MAX_POOLS_PER_REQUEST {
            return Err(ContractError::InvalidPools);
        }
        let mut pool_ids = HashSet::with_capacity(self.pools.len());
        let mut addresses = HashSet::with_capacity(self.pools.len());
        for pool in &self.pools {
            if !bounded(&pool.pool_id, 1, 256)
                || !canonical_address(&pool.address)
                || !bounded(&pool.protocol, 1, 64)
                || !pool.protocol.ends_with("V3")
                || !canonical_address(&pool.token0)
                || !canonical_address(&pool.token1)
                || pool.token0 == pool.token1
                || !(1..=36).contains(&pool.token0_decimals)
                || !(1..=36).contains(&pool.token1_decimals)
                || pool.fee == 0
                || pool.fee >= 1_000_000
                || !(1..=887_272).contains(&pool.tick_spacing)
                || !pool_ids.insert(pool.pool_id.as_str())
                || !addresses.insert(pool.address.as_str())
            {
                return Err(ContractError::InvalidPools);
            }
        }
        if let EvidenceRequest::Verify {
            block_number,
            block_hash,
            primary_state_hash,
        } = &self.evidence
        {
            if *block_number == 0
                || !canonical_block_hash(block_hash)
                || !canonical_digest(primary_state_hash)
            {
                return Err(ContractError::InvalidEvidence);
            }
        }
        Ok(())
    }

    pub fn canonical_hash(&self) -> Result<String, ContractError> {
        self.validate()?;
        let encoded = serde_json::to_vec(self).map_err(|_| ContractError::InvalidRoute)?;
        Ok(hex::encode(Sha256::digest(encoded)))
    }

    pub fn route_config_hash(&self) -> Result<String, ContractError> {
        self.validate()?;
        let encoded = serde_json::to_vec(&(&self.route_fingerprint, &self.pools))
            .map_err(|_| ContractError::InvalidRoute)?;
        Ok(hex::encode(Sha256::digest(encoded)))
    }
}

pub fn canonical_hash_bytes(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}

pub fn canonical_hash_str(value: &str) -> String {
    canonical_hash_bytes(value.as_bytes())
}

pub fn canonical_block_hash(value: &str) -> bool {
    canonical_hex(value, 32)
}

pub fn canonical_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn canonical_data(value: &str, max_bytes: usize) -> bool {
    let Some(body) = value.strip_prefix("0x") else {
        return false;
    };
    body.len() % 2 == 0
        && body.len() / 2 <= max_bytes
        && body
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn canonical_address(value: &str) -> bool {
    canonical_hex(value, 20)
}

fn canonical_hex(value: &str, bytes: usize) -> bool {
    value.len() == 2 + bytes * 2
        && value.starts_with("0x")
        && value[2..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn bounded(value: &str, minimum: usize, maximum: usize) -> bool {
    value.len() >= minimum && value.len() <= maximum && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ShadowStateRequest {
        ShadowStateRequest {
            schema_version: SHADOW_STATE_SCHEMA_VERSION.to_string(),
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            route_fingerprint: "weth-usdc-v1".to_string(),
            pools: vec![
                PoolStateRequest {
                    pool_id: "pool-a".to_string(),
                    address: "0x1111111111111111111111111111111111111111".to_string(),
                    protocol: "UniswapV3".to_string(),
                    token0: "0x3333333333333333333333333333333333333333".to_string(),
                    token1: "0x4444444444444444444444444444444444444444".to_string(),
                    token0_decimals: 18,
                    token1_decimals: 6,
                    fee: 500,
                    tick_spacing: 10,
                },
                PoolStateRequest {
                    pool_id: "pool-b".to_string(),
                    address: "0x2222222222222222222222222222222222222222".to_string(),
                    protocol: "SushiSwapV3".to_string(),
                    token0: "0x3333333333333333333333333333333333333333".to_string(),
                    token1: "0x4444444444444444444444444444444444444444".to_string(),
                    token0_decimals: 18,
                    token1_decimals: 6,
                    fee: 500,
                    tick_spacing: 10,
                },
            ],
            evidence: EvidenceRequest::Primary,
        }
    }

    #[test]
    fn request_contract_is_bounded_and_deterministically_hashed() {
        let request = request();
        assert_eq!(request.validate(), Ok(()));
        assert_eq!(request.canonical_hash(), request.canonical_hash());
        assert_eq!(request.canonical_hash().unwrap().len(), 64);
    }

    #[test]
    fn duplicate_or_noncanonical_pool_targets_fail_closed() {
        let mut duplicate = request();
        let (first, rest) = duplicate.pools.split_at_mut(1);
        rest[0].address.clone_from(&first[0].address);
        assert_eq!(duplicate.validate(), Err(ContractError::InvalidPools));

        let mut uppercase = request();
        uppercase.pools[0].address = "0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string();
        assert_eq!(uppercase.validate(), Err(ContractError::InvalidPools));
    }

    #[test]
    fn verification_stage_requires_an_exact_block_and_primary_state_hash() {
        let mut verify = request();
        verify.evidence = EvidenceRequest::Verify {
            block_number: 100,
            block_hash: format!("0x{}", "a".repeat(64)),
            primary_state_hash: "b".repeat(64),
        };
        assert_eq!(verify.validate(), Ok(()));
        let mut malformed = verify;
        malformed.evidence = EvidenceRequest::Verify {
            block_number: 0,
            block_hash: "latest".to_string(),
            primary_state_hash: "not-a-hash".to_string(),
        };
        assert_eq!(malformed.validate(), Err(ContractError::InvalidEvidence));
    }

    #[test]
    fn response_material_helpers_reject_ambiguous_hex() {
        assert!(canonical_block_hash(&format!("0x{}", "a".repeat(64))));
        assert!(!canonical_block_hash("latest"));
        assert!(canonical_data("0x1234", 2));
        assert!(!canonical_data("0xABCDEF", 3));
        assert!(!canonical_data("0x1234", 1));
    }
}
