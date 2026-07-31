use crate::domain::Direction;
use crate::engine_input::EngineInput;
use crate::origin::OriginEvent;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const SOURCE_IDENTITY_SCHEMA_VERSION: &str = "phoenix.source-identity.v1";
pub const UNISWAP_V3_FACTORY_ARBITRUM: &str = "0x1f98431c8ad98523631ae4a59f267346ea31f984";
const MAX_PATH_ITEMS: usize = 8;
const MAX_COMMANDS: usize = 32;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceIdentity {
    pub schema_version: String,
    pub source_event_identity: String,
    pub source_chain_id: u64,
    pub source_transaction_hash: String,
    pub source_feed_sequence: u64,
    pub source_feed_order_position: Option<u64>,
    pub source_block_number: Option<u64>,
    pub source_block_hash: Option<String>,
    pub source_transaction_index: Option<u64>,
    pub source_command_index: u16,
    pub source_event_index: Option<u64>,
    pub source_observed_at_unix_ms: u64,
    pub source_router: String,
    pub source_factory: String,
    pub source_pool: String,
    pub source_pool_path: Vec<String>,
    pub source_token_path: Vec<String>,
    pub source_encoded_token_path: String,
    pub source_fee_path: Vec<u32>,
    pub source_direction: String,
    pub source_input_amount: String,
    pub decoded_commands: Vec<String>,
    pub unavailable_reason: String,
    pub source_identity_hash: String,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum SourceIdentityError {
    #[error("source identity input is incomplete")]
    Incomplete,
    #[error("source identity input is invalid")]
    Invalid,
    #[error("source identity hash is inconsistent")]
    HashMismatch,
}

impl SourceIdentity {
    pub fn from_event(
        input: &EngineInput,
        origin: &OriginEvent,
    ) -> Result<Self, SourceIdentityError> {
        let source_feed_order_position = input.normalized.source_feed_order_position;
        let unavailable_reason = if source_feed_order_position.is_some() {
            "awaiting_canonical_block_assignment"
        } else {
            "legacy_event_missing_order_position"
        }
        .to_string();
        let mut value = Self {
            schema_version: SOURCE_IDENTITY_SCHEMA_VERSION.to_string(),
            source_event_identity: input.identity.source_event_identity.clone(),
            source_chain_id: input.identity.chain_id,
            source_transaction_hash: input.identity.tx_hash.clone(),
            source_feed_sequence: input.identity.source_sequence,
            source_feed_order_position,
            source_block_number: None,
            source_block_hash: None,
            source_transaction_index: None,
            source_command_index: origin.source_command_index,
            source_event_index: None,
            source_observed_at_unix_ms: input.observed_at_unix_ms,
            source_router: origin.router.as_str().to_string(),
            source_factory: UNISWAP_V3_FACTORY_ARBITRUM.to_string(),
            source_pool: origin
                .candidate_touched_pools
                .first()
                .ok_or(SourceIdentityError::Incomplete)?
                .0
                .clone(),
            source_pool_path: origin
                .candidate_touched_pools
                .iter()
                .map(|pool| pool.0.clone())
                .collect(),
            source_token_path: origin
                .swap_path
                .iter()
                .map(|token| token.0.as_str().to_string())
                .collect(),
            source_encoded_token_path: origin.encoded_token_path.clone(),
            source_fee_path: origin.fee_path.clone(),
            source_direction: match origin.initiating_swap_direction() {
                Some(Direction::ZeroForOne) => "zero_for_one",
                Some(Direction::OneForZero) => "one_for_zero",
                None => return Err(SourceIdentityError::Invalid),
            }
            .to_string(),
            source_input_amount: origin.amount.0.to_string(),
            decoded_commands: origin.decoded_commands.clone(),
            unavailable_reason,
            source_identity_hash: "0".repeat(64),
        };
        value.source_identity_hash = value.canonical_hash()?;
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), SourceIdentityError> {
        if self.schema_version != SOURCE_IDENTITY_SCHEMA_VERSION
            || self.source_chain_id != 42161
            || self.source_event_identity.is_empty()
            || self.source_event_identity.len() > 200
            || !canonical_hex(&self.source_transaction_hash, 32)
            || self.source_feed_sequence == 0
            || self.source_observed_at_unix_ms == 0
            || !canonical_hex(&self.source_router, 20)
            || !canonical_hex(&self.source_factory, 20)
            || self.source_pool.is_empty()
            || self.source_pool.len() > 256
            || self.source_pool_path.is_empty()
            || self.source_pool_path.len() > MAX_PATH_ITEMS
            || self.source_pool_path.first() != Some(&self.source_pool)
            || self.source_token_path.len() != self.source_fee_path.len() + 1
            || self.source_token_path.len() < 2
            || self.source_token_path.len() > MAX_PATH_ITEMS + 1
            || self
                .source_token_path
                .iter()
                .any(|value| !canonical_hex(value, 20))
            || self
                .source_fee_path
                .iter()
                .any(|fee| *fee == 0 || *fee > 1_000_000)
            || !canonical_variable_hex(&self.source_encoded_token_path)
            || self.source_encoded_token_path
                != encode_v3_path(&self.source_token_path, &self.source_fee_path)?
            || self.source_pool_path
                != canonical_pool_path(&self.source_token_path, &self.source_fee_path)?
            || self.source_input_amount == "0"
            || !decimal(&self.source_input_amount)
            || !matches!(
                self.source_direction.as_str(),
                "zero_for_one" | "one_for_zero"
            )
            || self.decoded_commands.is_empty()
            || self.decoded_commands.len() > MAX_COMMANDS
            || self
                .decoded_commands
                .iter()
                .any(|command| command.is_empty() || command.len() > 64)
        {
            return Err(SourceIdentityError::Invalid);
        }
        if self.source_block_number.is_some()
            || self.source_block_hash.is_some()
            || self.source_transaction_index.is_some()
            || self.source_event_index.is_some()
        {
            return Err(SourceIdentityError::Invalid);
        }
        let reason_matches_order = matches!(
            (
                self.source_feed_order_position,
                self.unavailable_reason.as_str()
            ),
            (Some(_), "awaiting_canonical_block_assignment")
                | (None, "legacy_event_missing_order_position")
        );
        if !reason_matches_order {
            return Err(SourceIdentityError::Invalid);
        }
        if self.source_identity_hash != self.canonical_hash()? {
            return Err(SourceIdentityError::HashMismatch);
        }
        Ok(())
    }

    pub fn canonical_hash(&self) -> Result<String, SourceIdentityError> {
        let mut material = self.clone();
        material.source_identity_hash = "0".repeat(64);
        let encoded = serde_json::to_vec(&material).map_err(|_| SourceIdentityError::Invalid)?;
        Ok(hex::encode(Sha256::digest(encoded)))
    }
}

fn canonical_hex(value: &str, byte_count: usize) -> bool {
    value.len() == 2 + byte_count * 2
        && value.starts_with("0x")
        && value[2..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn canonical_variable_hex(value: &str) -> bool {
    value.starts_with("0x")
        && value.len() >= 4
        && value.len() % 2 == 0
        && value[2..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn decimal(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn canonical_pool_path(
    tokens: &[String],
    fees: &[u32],
) -> Result<Vec<String>, SourceIdentityError> {
    if tokens.len() != fees.len() + 1 {
        return Err(SourceIdentityError::Invalid);
    }
    tokens
        .windows(2)
        .zip(fees)
        .map(|(pair, fee)| {
            if !canonical_hex(&pair[0], 20) || !canonical_hex(&pair[1], 20) || pair[0] == pair[1] {
                return Err(SourceIdentityError::Invalid);
            }
            let (token0, token1) = if pair[0] < pair[1] {
                (&pair[0], &pair[1])
            } else {
                (&pair[1], &pair[0])
            };
            Ok(format!("{token0}:{token1}:{fee}"))
        })
        .collect()
}

fn encode_v3_path(tokens: &[String], fees: &[u32]) -> Result<String, SourceIdentityError> {
    if tokens.len() != fees.len() + 1 || fees.is_empty() {
        return Err(SourceIdentityError::Invalid);
    }
    let mut encoded = tokens[0].clone();
    for (fee, token) in fees.iter().zip(&tokens[1..]) {
        if !canonical_hex(token, 20) || *fee == 0 || *fee > 1_000_000 {
            return Err(SourceIdentityError::Invalid);
        }
        encoded.push_str(&format!("{fee:06x}"));
        encoded.push_str(&token[2..]);
    }
    Ok(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Address, Amount, ChainId, PoolId, SequenceNumber, TokenAddress, TxHash};
    use crate::engine_input::{EngineInput, InputIdentity};
    use crate::messaging::NormalizedTx;
    use money_path_classifier::{
        DecodedSwapKind, OriginEvidence, OuterSelectorKind, RouterKind, UnsupportedReason,
        WrapperKind,
    };
    use serde_json::json;

    fn fixture() -> (EngineInput, OriginEvent) {
        let token0 = "0x1111111111111111111111111111111111111111";
        let token1 = "0x2222222222222222222222222222222222222222";
        let input = EngineInput {
            identity: InputIdentity {
                source_event_identity: format!("phoenix.engine.input.v1:7:0x{}", "a".repeat(64)),
                source_sequence: 7,
                tx_hash: format!("0x{}", "a".repeat(64)),
                chain_id: 42161,
            },
            normalized: NormalizedTx {
                sequence: SequenceNumber(7),
                source_feed_order_position: Some(3),
                tx_hash: TxHash(format!("0x{}", "a".repeat(64))),
                tx_type: "0x02".to_string(),
                chain_id: ChainId(42161),
                from: Address::parse("0x3333333333333333333333333333333333333333").unwrap(),
                to: Some(Address::parse("0xe592427a0aece92de3edee1f18e0157c05861564").unwrap()),
                nonce: 1,
                value: "0".to_string(),
                calldata: "0x1234".to_string(),
                gas_limit: "1".to_string(),
                max_fee_per_gas: "1".to_string(),
                max_priority_fee_per_gas: "1".to_string(),
            },
            observed_at_unix_ms: 1_700_000_000_000,
            ingested_at_unix_ns: 1_700_000_000_000_000_000,
            canonical_payload: json!({}),
        };
        let origin = OriginEvent {
            origin_tx_hash: TxHash(format!("0x{}", "a".repeat(64))),
            origin_sequence: SequenceNumber(7),
            router: Address::parse("0xe592427a0aece92de3edee1f18e0157c05861564").unwrap(),
            decoded_commands: vec!["exactInputSingle".to_string()],
            source_command_index: 0,
            swap_path: vec![
                TokenAddress(Address::parse(token0).unwrap()),
                TokenAddress(Address::parse(token1).unwrap()),
            ],
            encoded_token_path: format!("{token0}0001f4{}", &token1[2..]),
            fee_path: vec![500],
            exact_in: true,
            amount: Amount(100),
            candidate_touched_pools: vec![PoolId(format!("{token0}:{token1}:500"))],
            classification_evidence: OriginEvidence {
                router_kind: Some(RouterKind::LegacySwapRouter),
                outer_selector_kind: OuterSelectorKind::LegacyExactInputSingle,
                wrapper_kind: WrapperKind::Direct,
                decoded_swap_kind: DecodedSwapKind::V3ExactInputSingle,
                command_count: 1,
                v3_hop_count: 1,
                exact_in: Some(true),
                supported: true,
                unsupported_reason: UnsupportedReason::None,
            },
        };
        (input, origin)
    }

    #[test]
    fn source_identity_is_deterministic_and_keeps_exact_path() {
        let (input, origin) = fixture();
        let first = SourceIdentity::from_event(&input, &origin).unwrap();
        let second = SourceIdentity::from_event(&input, &origin).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.source_feed_order_position, Some(3));
        assert_eq!(first.source_fee_path, vec![500]);
        assert_eq!(first.source_token_path.len(), 2);
        assert_eq!(first.source_identity_hash.len(), 64);
    }

    #[test]
    fn block_fields_cannot_be_fabricated_in_original_identity() {
        let (input, origin) = fixture();
        let mut value = SourceIdentity::from_event(&input, &origin).unwrap();
        assert_eq!(value.source_block_number, None);
        assert_eq!(value.source_block_hash, None);
        assert_eq!(value.source_transaction_index, None);
        assert_eq!(value.source_event_index, None);
        value.source_block_number = Some(1);
        value.source_block_hash = Some(format!("0x{}", "b".repeat(64)));
        assert_eq!(value.validate(), Err(SourceIdentityError::Invalid));
    }
}
