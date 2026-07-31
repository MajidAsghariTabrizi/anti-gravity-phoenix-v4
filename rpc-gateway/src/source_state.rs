use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tiny_keccak::{Hasher, Keccak};

pub const SOURCE_EVIDENCE_REQUEST_SCHEMA: &str = "phoenix.rpc.source-evidence-request.v1";
pub const SOURCE_EVIDENCE_RESPONSE_SCHEMA: &str = "phoenix.rpc.source-evidence-response.v1";
pub const MAX_SOURCE_HOPS: usize = 8;
pub const UNISWAP_V3_FACTORY_ARBITRUM: &str = "0x1f98431c8ad98523631ae4a59f267346ea31f984";
const UNISWAP_V3_POOL_INIT_CODE_HASH: &str =
    "e34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceEvidenceRequest {
    pub schema_version: String,
    pub source_event_identity: String,
    pub source_identity_hash: String,
    pub source_transaction_hash: String,
    pub source_router: String,
    pub source_factory: String,
    pub source_feed_sequence: u64,
    pub source_feed_order_position: u64,
    pub source_command_index: u16,
    pub source_pool_path: Vec<String>,
    pub source_token_path: Vec<String>,
    pub source_encoded_token_path: String,
    pub source_fee_path: Vec<u32>,
    pub state_reconstruction_required: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SourceEvidenceResponse {
    pub schema_version: String,
    pub source_event_identity: String,
    pub source_identity_hash: String,
    pub source_chain_id: u64,
    pub source_transaction_hash: String,
    pub source_feed_sequence: u64,
    pub source_feed_order_position: u64,
    pub source_command_index: u16,
    pub source_pool_path: Vec<String>,
    pub source_token_path: Vec<String>,
    pub source_encoded_token_path: String,
    pub source_fee_path: Vec<u32>,
    pub source_block_number: u64,
    pub source_block_hash: String,
    pub source_transaction_index: u64,
    pub source_event_index: Option<u64>,
    pub source_pool_addresses: Vec<String>,
    pub transaction_status: String,
    pub parent_block_number: u64,
    pub parent_block_hash: String,
    pub provider_id: String,
    pub provider_response_hash: String,
    pub enrichment_hash: String,
    pub reconstruction_method: String,
    pub post_initiating_state_hash: Option<String>,
    pub completeness_status: String,
    pub failure_reason: Option<String>,
    pub state_evidence: Value,
    pub evidence_hash: String,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum SourceEvidenceError {
    #[error("source evidence contract is invalid")]
    Invalid,
    #[error("source evidence hash is inconsistent")]
    HashMismatch,
}

impl SourceEvidenceRequest {
    pub fn validate(&self) -> Result<(), SourceEvidenceError> {
        if self.schema_version != SOURCE_EVIDENCE_REQUEST_SCHEMA
            || self.source_event_identity.is_empty()
            || self.source_event_identity.len() > 200
            || !canonical_hash(&self.source_identity_hash)
            || !canonical_prefixed_hash(&self.source_transaction_hash)
            || !canonical_address(&self.source_router)
            || self.source_factory != UNISWAP_V3_FACTORY_ARBITRUM
            || self.source_feed_sequence == 0
            || self.source_fee_path.is_empty()
            || self.source_fee_path.len() > MAX_SOURCE_HOPS
            || self.source_token_path.len() != self.source_fee_path.len() + 1
            || self.source_pool_path.len() != self.source_fee_path.len()
            || self
                .source_token_path
                .iter()
                .any(|token| !canonical_address(token))
            || self
                .source_fee_path
                .iter()
                .any(|fee| *fee == 0 || *fee > 1_000_000)
            || self.source_encoded_token_path
                != encode_v3_path(&self.source_token_path, &self.source_fee_path)?
            || self.source_pool_path
                != canonical_pool_path(&self.source_token_path, &self.source_fee_path)?
        {
            return Err(SourceEvidenceError::Invalid);
        }
        Ok(())
    }
}

impl SourceEvidenceResponse {
    pub fn validate(&self, request: &SourceEvidenceRequest) -> Result<(), SourceEvidenceError> {
        request.validate()?;
        if self.schema_version != SOURCE_EVIDENCE_RESPONSE_SCHEMA
            || self.source_event_identity != request.source_event_identity
            || self.source_identity_hash != request.source_identity_hash
            || self.source_chain_id != 42161
            || self.source_transaction_hash != request.source_transaction_hash
            || self.source_feed_sequence != request.source_feed_sequence
            || self.source_feed_order_position != request.source_feed_order_position
            || self.source_command_index != request.source_command_index
            || self.source_pool_path != request.source_pool_path
            || self.source_token_path != request.source_token_path
            || self.source_encoded_token_path != request.source_encoded_token_path
            || self.source_fee_path != request.source_fee_path
            || self.source_block_number == 0
            || !canonical_prefixed_hash(&self.source_block_hash)
            || !canonical_prefixed_hash(&self.parent_block_hash)
            || self.parent_block_number + 1 != self.source_block_number
            || !matches!(self.transaction_status.as_str(), "success" | "reverted")
            || self.provider_id.is_empty()
            || self.provider_id.len() > 128
            || !canonical_hash(&self.provider_response_hash)
            || !canonical_hash(&self.enrichment_hash)
            || self.source_pool_addresses.len() > MAX_SOURCE_HOPS
            || self
                .source_pool_addresses
                .iter()
                .any(|address| !canonical_address(address))
            || !self.state_evidence.is_object()
            || !canonical_hash(&self.evidence_hash)
        {
            return Err(SourceEvidenceError::Invalid);
        }
        let exact_source_event = self.transaction_status == "success"
            && self.source_event_index.is_some()
            && self.source_pool_addresses
                == expected_uniswap_v3_pool_addresses(
                    &request.source_factory,
                    &request.source_token_path,
                    &request.source_fee_path,
                )?;
        let reverted_without_event = self.transaction_status == "reverted"
            && self.source_event_index.is_none()
            && self.source_pool_addresses.is_empty();
        if !exact_source_event && !reverted_without_event {
            return Err(SourceEvidenceError::Invalid);
        }
        let complete = self.completeness_status == "complete"
            && self.reconstruction_method == "debug_trace_transaction_prestate_diff"
            && exact_source_event
            && self
                .post_initiating_state_hash
                .as_deref()
                .is_some_and(canonical_hash)
            && self.failure_reason.is_none();
        let incomplete = self.completeness_status == "incomplete"
            && self.reconstruction_method == "unavailable"
            && self.post_initiating_state_hash.is_none()
            && self
                .failure_reason
                .as_deref()
                .is_some_and(|reason| !reason.is_empty() && reason.len() <= 128);
        if !complete && !incomplete {
            return Err(SourceEvidenceError::Invalid);
        }
        if complete {
            let transitions = self
                .state_evidence
                .get("pool_state_transitions")
                .and_then(Value::as_object)
                .ok_or(SourceEvidenceError::Invalid)?;
            let transitions_match = transitions.len() == self.source_pool_addresses.len()
                && self.source_pool_addresses.iter().all(|address| {
                    transitions
                        .get(address)
                        .and_then(Value::as_object)
                        .is_some_and(|transition| {
                            transition.contains_key("pre")
                                && transition.contains_key("post")
                                && transition.contains_key("diff_pre")
                                && transition.contains_key("diff_post")
                        })
                });
            let prestate_hash = self
                .state_evidence
                .get("prestate_hash")
                .and_then(Value::as_str)
                .filter(|value| canonical_hash(value))
                .ok_or(SourceEvidenceError::Invalid)?;
            let state_diff_hash = self
                .state_evidence
                .get("state_diff_hash")
                .and_then(Value::as_str)
                .filter(|value| canonical_hash(value))
                .ok_or(SourceEvidenceError::Invalid)?;
            let expected_trace_hash = hash_json(&serde_json::json!({
                "prestate_hash": prestate_hash,
                "state_diff_hash": state_diff_hash
            }))?;
            let trace_hash_valid = self
                .state_evidence
                .get("trace_response_hash")
                .and_then(Value::as_str)
                == Some(expected_trace_hash.as_str());
            let binding_matches = self.state_evidence.get("source_transaction_hash")
                == Some(&Value::String(self.source_transaction_hash.clone()))
                && self.state_evidence.get("source_block_number")
                    == Some(&Value::from(self.source_block_number))
                && self.state_evidence.get("source_block_hash")
                    == Some(&Value::String(self.source_block_hash.clone()))
                && self.state_evidence.get("source_transaction_index")
                    == Some(&Value::from(self.source_transaction_index))
                && self.state_evidence.get("source_feed_sequence")
                    == Some(&Value::from(self.source_feed_sequence))
                && self.state_evidence.get("source_feed_order_position")
                    == Some(&Value::from(self.source_feed_order_position))
                && self.state_evidence.get("source_command_index")
                    == Some(&Value::from(self.source_command_index))
                && self.state_evidence.get("parent_block_number")
                    == Some(&Value::from(self.parent_block_number))
                && self.state_evidence.get("parent_block_hash")
                    == Some(&Value::String(self.parent_block_hash.clone()))
                && self.state_evidence.get("source_factory")
                    == Some(&Value::String(request.source_factory.clone()))
                && self.state_evidence.get("source_pool_path")
                    == Some(&serde_json::json!(self.source_pool_path))
                && self.state_evidence.get("source_token_path")
                    == Some(&serde_json::json!(self.source_token_path))
                && self.state_evidence.get("source_encoded_token_path")
                    == Some(&Value::String(self.source_encoded_token_path.clone()))
                && self.state_evidence.get("source_fee_path")
                    == Some(&serde_json::json!(self.source_fee_path))
                && self.state_evidence.get("source_pool_addresses")
                    == Some(&serde_json::json!(self.source_pool_addresses));
            let expected_post_state_hash = hash_json(&serde_json::json!({
                "schema_version": "phoenix.post-initiating-state.v1",
                "source_event_identity": self.source_event_identity,
                "source_identity_hash": self.source_identity_hash,
                "source_transaction_hash": self.source_transaction_hash,
                "source_feed_sequence": self.source_feed_sequence,
                "source_feed_order_position": self.source_feed_order_position,
                "source_command_index": self.source_command_index,
                "source_block_number": self.source_block_number,
                "source_block_hash": self.source_block_hash,
                "source_transaction_index": self.source_transaction_index,
                "parent_block_number": self.parent_block_number,
                "parent_block_hash": self.parent_block_hash,
                "source_factory": request.source_factory,
                "source_pool_path": self.source_pool_path,
                "source_token_path": self.source_token_path,
                "source_encoded_token_path": self.source_encoded_token_path,
                "source_fee_path": self.source_fee_path,
                "source_pool_addresses": self.source_pool_addresses,
                "prestate_hash": prestate_hash,
                "state_diff_hash": state_diff_hash,
                "pool_state_transitions": transitions
            }))?;
            if self
                .state_evidence
                .get("schema_version")
                .and_then(Value::as_str)
                != Some("phoenix.transaction-boundary-state.v1")
                || self.state_evidence.get("complete").and_then(Value::as_bool) != Some(true)
                || !transitions_match
                || !trace_hash_valid
                || !binding_matches
                || self.post_initiating_state_hash.as_deref()
                    != Some(expected_post_state_hash.as_str())
            {
                return Err(SourceEvidenceError::Invalid);
            }
        } else {
            let incomplete_binding_matches = self.state_evidence.get("source_transaction_hash")
                == Some(&Value::String(self.source_transaction_hash.clone()))
                && self.state_evidence.get("source_block_number")
                    == Some(&Value::from(self.source_block_number))
                && self.state_evidence.get("source_block_hash")
                    == Some(&Value::String(self.source_block_hash.clone()))
                && self.state_evidence.get("source_transaction_index")
                    == Some(&Value::from(self.source_transaction_index))
                && self.state_evidence.get("source_feed_order_position")
                    == Some(&Value::from(self.source_feed_order_position));
            if self
                .state_evidence
                .get("schema_version")
                .and_then(Value::as_str)
                != Some("phoenix.transaction-boundary-state.v1")
                || self.state_evidence.get("complete").and_then(Value::as_bool) != Some(false)
                || self
                    .state_evidence
                    .get("failure_reason")
                    .and_then(Value::as_str)
                    != self.failure_reason.as_deref()
                || !incomplete_binding_matches
            {
                return Err(SourceEvidenceError::Invalid);
            }
        }
        if self.enrichment_hash != self.canonical_enrichment_hash()?
            || self.evidence_hash != self.canonical_evidence_hash()?
        {
            return Err(SourceEvidenceError::HashMismatch);
        }
        Ok(())
    }

    pub fn canonical_enrichment_hash(&self) -> Result<String, SourceEvidenceError> {
        hash_json(&serde_json::json!({
            "schema_version": "phoenix.source-block-enrichment.v1",
            "source_event_identity": self.source_event_identity,
            "source_identity_hash": self.source_identity_hash,
            "source_chain_id": self.source_chain_id,
            "source_transaction_hash": self.source_transaction_hash,
            "source_feed_sequence": self.source_feed_sequence,
            "source_feed_order_position": self.source_feed_order_position,
            "source_command_index": self.source_command_index,
            "source_pool_path": self.source_pool_path,
            "source_token_path": self.source_token_path,
            "source_encoded_token_path": self.source_encoded_token_path,
            "source_fee_path": self.source_fee_path,
            "source_block_number": self.source_block_number,
            "source_block_hash": self.source_block_hash,
            "source_transaction_index": self.source_transaction_index,
            "source_event_index": self.source_event_index,
            "source_pool_addresses": self.source_pool_addresses,
            "transaction_status": self.transaction_status,
            "provider_id": self.provider_id,
            "provider_response_hash": self.provider_response_hash
        }))
    }

    pub fn canonical_evidence_hash(&self) -> Result<String, SourceEvidenceError> {
        hash_json(&serde_json::json!({
            "schema_version": self.schema_version,
            "source_event_identity": self.source_event_identity,
            "source_identity_hash": self.source_identity_hash,
            "enrichment_hash": self.enrichment_hash,
            "source_feed_sequence": self.source_feed_sequence,
            "source_feed_order_position": self.source_feed_order_position,
            "source_command_index": self.source_command_index,
            "source_pool_path": self.source_pool_path,
            "source_token_path": self.source_token_path,
            "source_encoded_token_path": self.source_encoded_token_path,
            "source_fee_path": self.source_fee_path,
            "source_block_number": self.source_block_number,
            "source_block_hash": self.source_block_hash,
            "source_transaction_hash": self.source_transaction_hash,
            "source_transaction_index": self.source_transaction_index,
            "parent_block_number": self.parent_block_number,
            "parent_block_hash": self.parent_block_hash,
            "reconstruction_method": self.reconstruction_method,
            "post_initiating_state_hash": self.post_initiating_state_hash,
            "completeness_status": self.completeness_status,
            "failure_reason": self.failure_reason,
            "provider_id": self.provider_id,
            "provider_response_hash": self.provider_response_hash,
            "state_evidence": self.state_evidence
        }))
    }
}

pub fn hash_json(value: &Value) -> Result<String, SourceEvidenceError> {
    let encoded = serde_json::to_vec(value).map_err(|_| SourceEvidenceError::Invalid)?;
    Ok(hex::encode(Sha256::digest(encoded)))
}

pub fn expected_uniswap_v3_pool_addresses(
    factory: &str,
    tokens: &[String],
    fees: &[u32],
) -> Result<Vec<String>, SourceEvidenceError> {
    if factory != UNISWAP_V3_FACTORY_ARBITRUM || tokens.len() != fees.len() + 1 {
        return Err(SourceEvidenceError::Invalid);
    }
    let factory = decode_fixed_hex(factory, 20)?;
    let init_code_hash = decode_fixed_hex(UNISWAP_V3_POOL_INIT_CODE_HASH, 32)?;
    tokens
        .windows(2)
        .zip(fees)
        .map(|(pair, fee)| {
            let mut first = decode_fixed_hex(&pair[0], 20)?;
            let mut second = decode_fixed_hex(&pair[1], 20)?;
            if first == second || *fee == 0 || *fee > 1_000_000 {
                return Err(SourceEvidenceError::Invalid);
            }
            if first > second {
                std::mem::swap(&mut first, &mut second);
            }
            let encoded = ethabi::encode(&[
                ethabi::Token::Address(ethabi::Address::from_slice(&first)),
                ethabi::Token::Address(ethabi::Address::from_slice(&second)),
                ethabi::Token::Uint(ethabi::Uint::from(*fee)),
            ]);
            let salt = keccak256(&encoded);
            let mut material = Vec::with_capacity(85);
            material.push(0xff);
            material.extend_from_slice(&factory);
            material.extend_from_slice(&salt);
            material.extend_from_slice(&init_code_hash);
            let address_hash = keccak256(&material);
            Ok(format!("0x{}", hex::encode(&address_hash[12..])))
        })
        .collect()
}

fn canonical_pool_path(
    tokens: &[String],
    fees: &[u32],
) -> Result<Vec<String>, SourceEvidenceError> {
    if tokens.len() != fees.len() + 1 {
        return Err(SourceEvidenceError::Invalid);
    }
    tokens
        .windows(2)
        .zip(fees)
        .map(|(pair, fee)| {
            if !canonical_address(&pair[0]) || !canonical_address(&pair[1]) || pair[0] == pair[1] {
                return Err(SourceEvidenceError::Invalid);
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

fn encode_v3_path(tokens: &[String], fees: &[u32]) -> Result<String, SourceEvidenceError> {
    if tokens.len() != fees.len() + 1 || fees.is_empty() {
        return Err(SourceEvidenceError::Invalid);
    }
    let mut encoded = tokens[0].clone();
    for (fee, token) in fees.iter().zip(&tokens[1..]) {
        if !canonical_address(token) || *fee == 0 || *fee > 1_000_000 {
            return Err(SourceEvidenceError::Invalid);
        }
        encoded.push_str(&format!("{fee:06x}"));
        encoded.push_str(&token[2..]);
    }
    Ok(encoded)
}

fn decode_fixed_hex(value: &str, bytes: usize) -> Result<Vec<u8>, SourceEvidenceError> {
    let raw = value.strip_prefix("0x").unwrap_or(value);
    if raw.len() != bytes * 2 {
        return Err(SourceEvidenceError::Invalid);
    }
    hex::decode(raw).map_err(|_| SourceEvidenceError::Invalid)
}

fn keccak256(value: &[u8]) -> [u8; 32] {
    let mut output = [0_u8; 32];
    let mut hasher = Keccak::v256();
    hasher.update(value);
    hasher.finalize(&mut output);
    output
}

fn canonical_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn canonical_prefixed_hash(value: &str) -> bool {
    value.strip_prefix("0x").is_some_and(canonical_hash)
}

fn canonical_address(value: &str) -> bool {
    value.len() == 42
        && value.starts_with("0x")
        && value[2..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WETH: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
    const USDC: &str = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";
    const POOL_500: &str = "0xc6962004f452be9203591991d15f6b388e09e8d0";

    fn request() -> SourceEvidenceRequest {
        SourceEvidenceRequest {
            schema_version: SOURCE_EVIDENCE_REQUEST_SCHEMA.to_string(),
            source_event_identity: "source".to_string(),
            source_identity_hash: "a".repeat(64),
            source_transaction_hash: format!("0x{}", "b".repeat(64)),
            source_router: "0x1111111111111111111111111111111111111111".to_string(),
            source_factory: UNISWAP_V3_FACTORY_ARBITRUM.to_string(),
            source_feed_sequence: 1,
            source_feed_order_position: 0,
            source_command_index: 0,
            source_pool_path: vec![format!("{WETH}:{USDC}:500")],
            source_token_path: vec![WETH.to_string(), USDC.to_string()],
            source_encoded_token_path: format!("{WETH}0001f4{}", &USDC[2..]),
            source_fee_path: vec![500],
            state_reconstruction_required: true,
        }
    }

    fn response(request: &SourceEvidenceRequest, complete: bool) -> SourceEvidenceResponse {
        let mut response = SourceEvidenceResponse {
            schema_version: SOURCE_EVIDENCE_RESPONSE_SCHEMA.to_string(),
            source_event_identity: request.source_event_identity.clone(),
            source_identity_hash: request.source_identity_hash.clone(),
            source_chain_id: 42161,
            source_transaction_hash: request.source_transaction_hash.clone(),
            source_feed_sequence: request.source_feed_sequence,
            source_feed_order_position: request.source_feed_order_position,
            source_command_index: request.source_command_index,
            source_pool_path: request.source_pool_path.clone(),
            source_token_path: request.source_token_path.clone(),
            source_encoded_token_path: request.source_encoded_token_path.clone(),
            source_fee_path: request.source_fee_path.clone(),
            source_block_number: 100,
            source_block_hash: format!("0x{}", "c".repeat(64)),
            source_transaction_index: 2,
            source_event_index: Some(7),
            source_pool_addresses: vec![POOL_500.to_string()],
            transaction_status: "success".to_string(),
            parent_block_number: 99,
            parent_block_hash: format!("0x{}", "d".repeat(64)),
            provider_id: "provider_0".to_string(),
            provider_response_hash: "e".repeat(64),
            enrichment_hash: "0".repeat(64),
            reconstruction_method: if complete {
                "debug_trace_transaction_prestate_diff"
            } else {
                "unavailable"
            }
            .to_string(),
            post_initiating_state_hash: None,
            completeness_status: if complete { "complete" } else { "incomplete" }.to_string(),
            failure_reason: (!complete).then(|| "transaction_trace_unavailable".to_string()),
            state_evidence: if complete {
                let prestate_hash = "1".repeat(64);
                let state_diff_hash = "2".repeat(64);
                serde_json::json!({
                    "schema_version": "phoenix.transaction-boundary-state.v1",
                    "complete": true,
                    "trace_response_hash": hash_json(&serde_json::json!({
                        "prestate_hash": prestate_hash,
                        "state_diff_hash": state_diff_hash
                    })).unwrap(),
                    "prestate_hash": prestate_hash,
                    "state_diff_hash": state_diff_hash,
                    "source_transaction_hash": request.source_transaction_hash,
                    "source_block_number": 100,
                    "source_block_hash": format!("0x{}", "c".repeat(64)),
                    "source_transaction_index": 2,
                    "source_feed_sequence": request.source_feed_sequence,
                    "source_feed_order_position": request.source_feed_order_position,
                    "source_command_index": request.source_command_index,
                    "parent_block_number": 99,
                    "parent_block_hash": format!("0x{}", "d".repeat(64)),
                    "source_factory": request.source_factory,
                    "source_pool_path": request.source_pool_path,
                    "source_token_path": request.source_token_path,
                    "source_encoded_token_path": request.source_encoded_token_path,
                    "source_fee_path": request.source_fee_path,
                    "source_pool_addresses": [POOL_500],
                    "pool_state_transitions": {
                        "0xc6962004f452be9203591991d15f6b388e09e8d0": {
                            "pre": {"storage": {}},
                            "post": {"storage": {}},
                            "diff_pre": {"storage": {}},
                            "diff_post": {"storage": {}}
                        }
                    }
                })
            } else {
                serde_json::json!({
                    "schema_version": "phoenix.transaction-boundary-state.v1",
                    "complete": false,
                    "failure_reason": "transaction_trace_unavailable",
                    "source_transaction_hash": request.source_transaction_hash,
                    "source_block_number": 100,
                    "source_block_hash": format!("0x{}", "c".repeat(64)),
                    "source_transaction_index": 2,
                    "source_feed_sequence": request.source_feed_sequence,
                    "source_feed_order_position": request.source_feed_order_position,
                    "source_command_index": request.source_command_index,
                    "parent_block_number": 99,
                    "parent_block_hash": format!("0x{}", "d".repeat(64)),
                    "source_factory": request.source_factory,
                    "source_pool_path": request.source_pool_path,
                    "source_token_path": request.source_token_path,
                    "source_encoded_token_path": request.source_encoded_token_path,
                    "source_fee_path": request.source_fee_path,
                    "source_pool_addresses": [POOL_500]
                })
            },
            evidence_hash: "0".repeat(64),
        };
        if complete {
            response.post_initiating_state_hash = Some(
                hash_json(&serde_json::json!({
                    "schema_version": "phoenix.post-initiating-state.v1",
                    "source_event_identity": response.source_event_identity,
                    "source_identity_hash": response.source_identity_hash,
                    "source_transaction_hash": response.source_transaction_hash,
                    "source_feed_sequence": response.source_feed_sequence,
                    "source_feed_order_position": response.source_feed_order_position,
                    "source_command_index": response.source_command_index,
                    "source_block_number": response.source_block_number,
                    "source_block_hash": response.source_block_hash,
                    "source_transaction_index": response.source_transaction_index,
                    "parent_block_number": response.parent_block_number,
                    "parent_block_hash": response.parent_block_hash,
                    "source_factory": request.source_factory,
                    "source_pool_path": response.source_pool_path,
                    "source_token_path": response.source_token_path,
                    "source_encoded_token_path": response.source_encoded_token_path,
                    "source_fee_path": response.source_fee_path,
                    "source_pool_addresses": response.source_pool_addresses,
                    "prestate_hash": response.state_evidence["prestate_hash"],
                    "state_diff_hash": response.state_evidence["state_diff_hash"],
                    "pool_state_transitions": response.state_evidence["pool_state_transitions"]
                }))
                .unwrap(),
            );
        }
        response.enrichment_hash = response.canonical_enrichment_hash().unwrap();
        response.evidence_hash = response.canonical_evidence_hash().unwrap();
        response
    }

    #[test]
    fn request_rejects_unbound_or_unbounded_identity() {
        let mut request = request();
        assert_eq!(request.validate(), Ok(()));
        request.source_fee_path = vec![500; MAX_SOURCE_HOPS + 1];
        assert_eq!(request.validate(), Err(SourceEvidenceError::Invalid));
    }

    #[test]
    fn request_rejects_wrong_factory_fee_and_reordered_path() {
        let mut wrong_factory = request();
        wrong_factory.source_factory = "0x1111111111111111111111111111111111111111".to_string();
        assert_eq!(wrong_factory.validate(), Err(SourceEvidenceError::Invalid));

        let mut wrong_fee = request();
        wrong_fee.source_fee_path = vec![3_000];
        assert_eq!(wrong_fee.validate(), Err(SourceEvidenceError::Invalid));

        let mut reordered_path = request();
        reordered_path.source_token_path.reverse();
        assert_eq!(reordered_path.validate(), Err(SourceEvidenceError::Invalid));
    }

    #[test]
    fn canonical_pool_address_and_full_path_are_bound() {
        let request = request();
        assert_eq!(
            expected_uniswap_v3_pool_addresses(
                &request.source_factory,
                &request.source_token_path,
                &request.source_fee_path
            ),
            Ok(vec![POOL_500.to_string()])
        );
        assert_eq!(response(&request, true).validate(&request), Ok(()));
        assert_eq!(response(&request, false).validate(&request), Ok(()));
    }

    #[test]
    fn response_rejects_wrong_pool_or_stale_block_binding() {
        let request = request();
        let mut wrong_pool = response(&request, true);
        wrong_pool.source_pool_addresses =
            vec!["0x2222222222222222222222222222222222222222".to_string()];
        wrong_pool.enrichment_hash = wrong_pool.canonical_enrichment_hash().unwrap();
        wrong_pool.evidence_hash = wrong_pool.canonical_evidence_hash().unwrap();
        assert_eq!(
            wrong_pool.validate(&request),
            Err(SourceEvidenceError::Invalid)
        );

        let mut stale_block = response(&request, true);
        stale_block.parent_block_number = 98;
        stale_block.evidence_hash = stale_block.canonical_evidence_hash().unwrap();
        assert_eq!(
            stale_block.validate(&request),
            Err(SourceEvidenceError::Invalid)
        );
    }

    #[test]
    fn response_rejects_wrong_transaction_and_mixed_block_state() {
        let request = request();
        let mut wrong_transaction = response(&request, true);
        wrong_transaction.source_transaction_hash = format!("0x{}", "9".repeat(64));
        wrong_transaction.enrichment_hash = wrong_transaction.canonical_enrichment_hash().unwrap();
        wrong_transaction.evidence_hash = wrong_transaction.canonical_evidence_hash().unwrap();
        assert_eq!(
            wrong_transaction.validate(&request),
            Err(SourceEvidenceError::Invalid)
        );

        let mut mixed_block = response(&request, true);
        mixed_block.state_evidence["source_block_hash"] =
            serde_json::json!(format!("0x{}", "8".repeat(64)));
        mixed_block.evidence_hash = mixed_block.canonical_evidence_hash().unwrap();
        assert_eq!(
            mixed_block.validate(&request),
            Err(SourceEvidenceError::Invalid)
        );
    }
}
