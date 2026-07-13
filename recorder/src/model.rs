use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

pub const NORMALIZED_SCHEMA_VERSION: &str = "phoenix.v4.normalized_tx.v1";
pub const ENGINE_INPUT_SCHEMA_VERSION: &str = "phoenix.engine.input.v1";
pub const ARBITRUM_ONE_CHAIN_ID: u64 = 42161;
pub const ORIGIN_CLASSIFICATION: &str = crate::OpportunityLifecycle::OriginSeen.as_str();
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
pub const MAX_TRANSACTION_BYTES: usize = 256 * 1024;

pub fn engine_event_identity(tx: &NormalizedTx) -> String {
    format!(
        "{}:{}:{}",
        ENGINE_INPUT_SCHEMA_VERSION, tx.sequence, tx.tx_hash
    )
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NormalizedTx {
    pub schema_version: String,
    pub sequence: u64,
    pub timestamp_unix_ms: u64,
    pub tx_hash: String,
    pub tx_type: String,
    pub chain_id: u64,
    pub from: String,
    pub to: String,
    pub nonce: u64,
    pub value: String,
    pub calldata: String,
    pub gas_limit: String,
    pub max_fee_per_gas: String,
    pub max_priority_fee_per_gas: String,
    pub raw_tx: String,
    pub ingested_at_unix_ns: i64,
}

#[derive(Clone, Debug)]
pub struct ValidatedMessage {
    pub tx: NormalizedTx,
    pub payload: Value,
    pub calldata: Vec<u8>,
    pub seen_at: DateTime<Utc>,
    pub metadata: Value,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("payload exceeds the recorder message limit")]
    Oversized,
    #[error("payload is not valid normalized transaction JSON")]
    InvalidJson,
    #[error("unsupported normalized transaction schema")]
    UnsupportedSchema,
    #[error("unsupported chain id")]
    UnsupportedChain,
    #[error("invalid transaction hash")]
    InvalidTransactionHash,
    #[error("invalid sender address")]
    InvalidSender,
    #[error("invalid destination address")]
    InvalidDestination,
    #[error("invalid transaction type")]
    InvalidTransactionType,
    #[error("invalid decimal transaction field")]
    InvalidDecimal,
    #[error("invalid calldata")]
    InvalidCalldata,
    #[error("invalid raw transaction encoding")]
    InvalidRawTransaction,
    #[error("invalid ingestion timestamp")]
    InvalidIngestionTimestamp,
}

pub fn decode_message(raw: &[u8]) -> Result<ValidatedMessage, DecodeError> {
    if raw.len() > MAX_MESSAGE_BYTES {
        return Err(DecodeError::Oversized);
    }

    let payload: Value = serde_json::from_slice(raw).map_err(|_| DecodeError::InvalidJson)?;
    if !payload.is_object() {
        return Err(DecodeError::InvalidJson);
    }
    let tx: NormalizedTx =
        serde_json::from_value(payload.clone()).map_err(|_| DecodeError::InvalidJson)?;

    if tx.schema_version != NORMALIZED_SCHEMA_VERSION {
        return Err(DecodeError::UnsupportedSchema);
    }
    if tx.chain_id != ARBITRUM_ONE_CHAIN_ID {
        return Err(DecodeError::UnsupportedChain);
    }
    if !is_canonical_hex(&tx.tx_hash, 32) {
        return Err(DecodeError::InvalidTransactionHash);
    }
    if !is_canonical_hex(&tx.from, 20) {
        return Err(DecodeError::InvalidSender);
    }
    if !tx.to.is_empty() && !is_canonical_hex(&tx.to, 20) {
        return Err(DecodeError::InvalidDestination);
    }
    if !is_canonical_hex(&tx.tx_type, 1) {
        return Err(DecodeError::InvalidTransactionType);
    }
    if [
        &tx.value,
        &tx.gas_limit,
        &tx.max_fee_per_gas,
        &tx.max_priority_fee_per_gas,
    ]
    .iter()
    .any(|value| !is_decimal(value))
    {
        return Err(DecodeError::InvalidDecimal);
    }

    let calldata = decode_hex(&tx.calldata).ok_or(DecodeError::InvalidCalldata)?;
    if calldata.len() > MAX_TRANSACTION_BYTES {
        return Err(DecodeError::InvalidCalldata);
    }
    let raw_transaction = BASE64_STANDARD
        .decode(&tx.raw_tx)
        .map_err(|_| DecodeError::InvalidRawTransaction)?;
    if raw_transaction.len() > MAX_TRANSACTION_BYTES {
        return Err(DecodeError::InvalidRawTransaction);
    }

    if tx.ingested_at_unix_ns <= 0 {
        return Err(DecodeError::InvalidIngestionTimestamp);
    }
    let seconds = tx.ingested_at_unix_ns.div_euclid(1_000_000_000);
    let nanos = tx.ingested_at_unix_ns.rem_euclid(1_000_000_000) as u32;
    let seen_at =
        DateTime::from_timestamp(seconds, nanos).ok_or(DecodeError::InvalidIngestionTimestamp)?;

    let metadata = json!({
        "schema_version": tx.schema_version,
        "source_subject": crate::NATS_SUBJECT,
        "feed_timestamp_unix_ms": tx.timestamp_unix_ms,
        "ingested_at_unix_ns": tx.ingested_at_unix_ns,
        "tx_type": tx.tx_type,
        "from": tx.from,
        "to": tx.to,
        "nonce": tx.nonce,
        "value": tx.value,
        "gas_limit": tx.gas_limit,
        "max_fee_per_gas": tx.max_fee_per_gas,
        "max_priority_fee_per_gas": tx.max_priority_fee_per_gas,
    });

    Ok(ValidatedMessage {
        tx,
        payload,
        calldata,
        seen_at,
        metadata,
    })
}

fn is_decimal(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_canonical_hex(value: &str, byte_count: usize) -> bool {
    value.len() == 2 + byte_count * 2
        && value.starts_with("0x")
        && value[2..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    let body = value.strip_prefix("0x")?;
    if body.len() % 2 != 0 || body.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return None;
    }
    hex::decode(body).ok()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn sample_payload(sequence: u64, hash_byte: char) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schema_version": NORMALIZED_SCHEMA_VERSION,
            "sequence": sequence,
            "timestamp_unix_ms": 1_700_000_000_000_u64,
            "tx_hash": format!("0x{}", hash_byte.to_string().repeat(64)),
            "tx_type": "0x02",
            "chain_id": ARBITRUM_ONE_CHAIN_ID,
            "from": "0x1111111111111111111111111111111111111111",
            "to": "0x2222222222222222222222222222222222222222",
            "nonce": 7,
            "value": "3",
            "calldata": "0x1234",
            "gas_limit": "21000",
            "max_fee_per_gas": "100",
            "max_priority_fee_per_gas": "2",
            "raw_tx": "AQID",
            "ingested_at_unix_ns": 1_700_000_000_123_456_789_i64
        }))
        .unwrap()
    }

    #[test]
    fn validates_exact_normalized_contract() {
        let message = decode_message(&sample_payload(7, 'a')).unwrap();
        assert_eq!(message.tx.sequence, 7);
        assert_eq!(message.calldata, vec![0x12, 0x34]);
        assert_eq!(message.metadata["source_subject"], crate::NATS_SUBJECT);
        assert!(message.metadata.get("raw_tx").is_none());
        assert_eq!(
            engine_event_identity(&message.tx),
            format!("{}:7:0x{}", ENGINE_INPUT_SCHEMA_VERSION, "a".repeat(64))
        );
        assert!(serde_json::to_vec(&message.payload).unwrap().len() <= MAX_MESSAGE_BYTES);
    }

    #[test]
    fn rejects_arrays_unknown_fields_and_malformed_values() {
        assert_eq!(decode_message(b"[]").unwrap_err(), DecodeError::InvalidJson);

        let mut value: Value = serde_json::from_slice(&sample_payload(1, 'a')).unwrap();
        value["unexpected"] = json!(true);
        assert_eq!(
            decode_message(&serde_json::to_vec(&value).unwrap()).unwrap_err(),
            DecodeError::InvalidJson
        );

        value.as_object_mut().unwrap().remove("unexpected");
        value["chain_id"] = json!(1);
        assert_eq!(
            decode_message(&serde_json::to_vec(&value).unwrap()).unwrap_err(),
            DecodeError::UnsupportedChain
        );
    }

    #[test]
    fn rejects_invalid_raw_transaction_without_echoing_it() {
        let mut value: Value = serde_json::from_slice(&sample_payload(1, 'a')).unwrap();
        value["raw_tx"] = json!("not-base64-or-secret-material");
        let error = decode_message(&serde_json::to_vec(&value).unwrap()).unwrap_err();
        assert_eq!(error, DecodeError::InvalidRawTransaction);
        assert!(!error.to_string().contains("secret-material"));
    }
}
