use crate::domain::{Address, ChainId, SequenceNumber, TxHash};
use crate::messaging::NormalizedTx;
use phoenix_recorder::model::{
    decode_message, engine_event_identity, ENGINE_INPUT_SCHEMA_VERSION, MAX_MESSAGE_BYTES,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineClassification {
    NoRelevantRoute,
    CandidateGenerated,
    CandidateRejected,
    ShadowAccepted,
    MalformedInternalEvent,
    UnsupportedSchema,
    TransientDependencyFailure,
    DependencyExhausted,
    TerminalIntegrityFailure,
}

impl EngineClassification {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoRelevantRoute => "no_relevant_route",
            Self::CandidateGenerated => "candidate_generated",
            Self::CandidateRejected => "candidate_rejected",
            Self::ShadowAccepted => "shadow_accepted",
            Self::MalformedInternalEvent => "malformed_internal_event",
            Self::UnsupportedSchema => "unsupported_schema",
            Self::TransientDependencyFailure => "transient_dependency_failure",
            Self::DependencyExhausted => "dependency_exhausted",
            Self::TerminalIntegrityFailure => "terminal_integrity_failure",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "no_relevant_route" => Some(Self::NoRelevantRoute),
            "candidate_generated" => Some(Self::CandidateGenerated),
            "candidate_rejected" => Some(Self::CandidateRejected),
            "shadow_accepted" => Some(Self::ShadowAccepted),
            "malformed_internal_event" => Some(Self::MalformedInternalEvent),
            "unsupported_schema" => Some(Self::UnsupportedSchema),
            "transient_dependency_failure" => Some(Self::TransientDependencyFailure),
            "dependency_exhausted" => Some(Self::DependencyExhausted),
            "terminal_integrity_failure" => Some(Self::TerminalIntegrityFailure),
            _ => None,
        }
    }

    pub const fn is_final(self) -> bool {
        !matches!(
            self,
            Self::CandidateGenerated
                | Self::MalformedInternalEvent
                | Self::TransientDependencyFailure
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputIdentity {
    pub source_event_identity: String,
    pub source_sequence: u64,
    pub tx_hash: String,
    pub chain_id: u64,
}

#[derive(Clone, Debug)]
pub struct EngineInput {
    pub identity: InputIdentity,
    pub normalized: NormalizedTx,
    pub observed_at_unix_ms: u64,
    pub ingested_at_unix_ns: i64,
    pub canonical_payload: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputFailureKind {
    UnsupportedSchema,
    Malformed,
}

impl InputFailureKind {
    pub const fn class(self) -> &'static str {
        match self {
            Self::UnsupportedSchema => "unsupported_schema",
            Self::Malformed => "malformed_internal_event",
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("Engine input failed internal contract validation")]
pub struct InputFailure {
    pub kind: InputFailureKind,
    pub identity: InputIdentity,
    pub evidence: Value,
}

pub fn decode_engine_input(
    payload: &[u8],
    schema_header: Option<&str>,
    identity_header: Option<&str>,
    stream_sequence: u64,
) -> Result<EngineInput, InputFailure> {
    let fallback = fallback_identity(payload, stream_sequence);
    if schema_header != Some(ENGINE_INPUT_SCHEMA_VERSION) {
        return Err(failure(
            InputFailureKind::UnsupportedSchema,
            fallback,
            payload.len(),
            schema_header.is_some(),
            identity_header.is_some(),
        ));
    }
    if payload.len() > MAX_MESSAGE_BYTES {
        return Err(failure(
            InputFailureKind::Malformed,
            fallback,
            payload.len(),
            true,
            identity_header.is_some(),
        ));
    }
    let validated = match decode_message(payload) {
        Ok(validated) => validated,
        Err(_) => {
            return Err(failure(
                InputFailureKind::Malformed,
                fallback,
                payload.len(),
                true,
                identity_header.is_some(),
            ));
        }
    };
    let expected_identity = engine_event_identity(&validated.tx);
    if identity_header != Some(expected_identity.as_str()) {
        return Err(failure(
            InputFailureKind::Malformed,
            InputIdentity {
                source_event_identity: expected_identity,
                source_sequence: validated.tx.sequence,
                tx_hash: validated.tx.tx_hash.clone(),
                chain_id: validated.tx.chain_id,
            },
            payload.len(),
            true,
            identity_header.is_some(),
        ));
    }

    let from = Address::parse(&validated.tx.from).map_err(|_| {
        failure(
            InputFailureKind::Malformed,
            fallback.clone(),
            payload.len(),
            true,
            true,
        )
    })?;
    let to = if validated.tx.to.is_empty() {
        None
    } else {
        Some(Address::parse(&validated.tx.to).map_err(|_| {
            failure(
                InputFailureKind::Malformed,
                fallback.clone(),
                payload.len(),
                true,
                true,
            )
        })?)
    };
    Ok(EngineInput {
        identity: InputIdentity {
            source_event_identity: expected_identity,
            source_sequence: validated.tx.sequence,
            tx_hash: validated.tx.tx_hash.clone(),
            chain_id: validated.tx.chain_id,
        },
        normalized: NormalizedTx {
            sequence: SequenceNumber(validated.tx.sequence),
            tx_hash: TxHash(validated.tx.tx_hash),
            tx_type: validated.tx.tx_type,
            chain_id: ChainId(validated.tx.chain_id),
            from,
            to,
            nonce: validated.tx.nonce,
            value: validated.tx.value,
            calldata: validated.tx.calldata,
            gas_limit: validated.tx.gas_limit,
            max_fee_per_gas: validated.tx.max_fee_per_gas,
            max_priority_fee_per_gas: validated.tx.max_priority_fee_per_gas,
        },
        observed_at_unix_ms: validated.tx.timestamp_unix_ms,
        ingested_at_unix_ns: validated.tx.ingested_at_unix_ns,
        canonical_payload: validated.payload,
    })
}

pub fn fallback_identity(payload: &[u8], stream_sequence: u64) -> InputIdentity {
    let digest = hex::encode(Sha256::digest(payload));
    InputIdentity {
        source_event_identity: format!("phoenix.engine.poison.v1:{stream_sequence}:{digest}"),
        source_sequence: stream_sequence,
        tx_hash: format!("0x{digest}"),
        chain_id: 42161,
    }
}

fn failure(
    kind: InputFailureKind,
    identity: InputIdentity,
    payload_bytes: usize,
    schema_header_present: bool,
    identity_header_present: bool,
) -> InputFailure {
    InputFailure {
        kind,
        identity,
        evidence: json!({
            "failure_class": kind.class(),
            "payload_bytes": payload_bytes,
            "schema_header_present": schema_header_present,
            "identity_header_present": identity_header_present
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoenix_recorder::model::{ARBITRUM_ONE_CHAIN_ID, NORMALIZED_SCHEMA_VERSION};

    fn sample_payload(sequence: u64, hash_byte: char) -> Vec<u8> {
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
    fn exact_recorder_event_and_headers_reconstruct_engine_input() {
        let payload = sample_payload(7, 'a');
        let validated = decode_message(&payload).unwrap();
        let identity = engine_event_identity(&validated.tx);
        let input = decode_engine_input(
            &payload,
            Some(ENGINE_INPUT_SCHEMA_VERSION),
            Some(&identity),
            11,
        )
        .unwrap();
        assert_eq!(input.identity.source_sequence, 7);
        assert_eq!(input.identity.source_event_identity, identity);
        assert_eq!(input.normalized.chain_id, ChainId(42161));
    }

    #[test]
    fn unsupported_schema_and_malformed_payload_have_stable_poison_identity() {
        let payload = b"not-json";
        let first = decode_engine_input(payload, Some("future-v2"), None, 9).unwrap_err();
        let second = decode_engine_input(payload, Some("future-v2"), None, 9).unwrap_err();
        assert_eq!(first.kind, InputFailureKind::UnsupportedSchema);
        assert_eq!(first.identity, second.identity);
        assert!(!first.evidence.to_string().contains("not-json"));

        let malformed = decode_engine_input(
            payload,
            Some(ENGINE_INPUT_SCHEMA_VERSION),
            Some("invalid"),
            9,
        )
        .unwrap_err();
        assert_eq!(malformed.kind, InputFailureKind::Malformed);
    }

    #[test]
    fn identity_header_must_match_canonical_event() {
        let payload = sample_payload(7, 'a');
        let failure = decode_engine_input(
            &payload,
            Some(ENGINE_INPUT_SCHEMA_VERSION),
            Some("phoenix.engine.input.v1:7:wrong"),
            11,
        )
        .unwrap_err();
        assert_eq!(failure.kind, InputFailureKind::Malformed);
    }

    #[test]
    fn classification_labels_match_database_contract() {
        for classification in [
            EngineClassification::NoRelevantRoute,
            EngineClassification::CandidateGenerated,
            EngineClassification::CandidateRejected,
            EngineClassification::ShadowAccepted,
            EngineClassification::MalformedInternalEvent,
            EngineClassification::UnsupportedSchema,
            EngineClassification::TransientDependencyFailure,
            EngineClassification::DependencyExhausted,
            EngineClassification::TerminalIntegrityFailure,
        ] {
            assert_eq!(
                EngineClassification::parse(classification.as_str()),
                Some(classification)
            );
        }
    }
}
