use crate::domain::Amount;
use crate::opportunity::{
    BasisPoints, SignedAmount, SimulationClassification, SimulationEvidence, SimulationKind,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimulationRequest {
    pub kind: SimulationKind,
    pub block_number: u64,
    pub block_hash: String,
    pub from_address: Option<String>,
    pub target_contract: String,
    pub expected_contract_code_hash: String,
    pub calldata_hash: String,
    pub value: Amount,
    pub state_overrides_hash: Option<String>,
    pub provider_id: String,
    pub requested_at_unix_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimulationResponse {
    pub block_number: u64,
    pub block_hash: String,
    pub contract_code_hash: String,
    pub gas_estimate: Option<u64>,
    pub gas_used: Option<u64>,
    pub output: Option<Amount>,
    pub net_pnl: Option<SignedAmount>,
    pub revert_reason: Option<String>,
    pub provider_id: String,
    pub completed_at_unix_ms: u64,
    pub latency_ns: u128,
    pub state_drift_bps: BasisPoints,
    pub provider_disagreement: bool,
    pub token_behavior_known_safe: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SimulationError {
    AmbiguousBlock,
    ProviderDisagreement,
    StaleState,
    ContractVersionMismatch,
    ContractUnavailable,
    UnsafeToken,
    Reverted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SimulationPolicy {
    pub max_age_ms: u64,
    pub max_state_drift_bps: BasisPoints,
}

pub fn verify(
    request: &SimulationRequest,
    response: &SimulationResponse,
    policy: SimulationPolicy,
) -> Result<SimulationEvidence, SimulationError> {
    if request.block_number == 0
        || request.block_hash.is_empty()
        || response.block_number != request.block_number
        || response.block_hash != request.block_hash
    {
        return Err(SimulationError::AmbiguousBlock);
    }
    if response.provider_disagreement || response.provider_id != request.provider_id {
        return Err(SimulationError::ProviderDisagreement);
    }
    if response
        .completed_at_unix_ms
        .saturating_sub(request.requested_at_unix_ms)
        > policy.max_age_ms
        || response.state_drift_bps > policy.max_state_drift_bps
    {
        return Err(SimulationError::StaleState);
    }
    if response.contract_code_hash.is_empty() {
        return Err(SimulationError::ContractUnavailable);
    }
    if response.contract_code_hash != request.expected_contract_code_hash {
        return Err(SimulationError::ContractVersionMismatch);
    }
    if !response.token_behavior_known_safe {
        return Err(SimulationError::UnsafeToken);
    }
    if response.revert_reason.is_some() {
        return Err(SimulationError::Reverted);
    }

    Ok(SimulationEvidence {
        kind: request.kind,
        block_number: request.block_number,
        block_hash: Some(request.block_hash.clone()),
        from_address: request.from_address.clone(),
        target_contract: Some(request.target_contract.clone()),
        contract_code_hash: Some(response.contract_code_hash.clone()),
        calldata_hash: request.calldata_hash.clone(),
        value: request.value,
        gas_estimate: response.gas_estimate,
        gas_used: response.gas_used,
        simulated_output: response.output,
        simulated_net_pnl: response.net_pnl,
        revert_reason: None,
        state_overrides_hash: request.state_overrides_hash.clone(),
        provider_id: Some(response.provider_id.clone()),
        simulated_at_unix_ms: response.completed_at_unix_ms,
        latency_ns: response.latency_ns,
        state_drift_bps: response.state_drift_bps,
        classification: SimulationClassification::Passed,
    })
}

pub fn proof_scope(kind: SimulationKind) -> (&'static str, &'static str) {
    match kind {
        SimulationKind::StaticQuote => (
            "quote arithmetic for the recorded inputs",
            "contract execution, inclusion, or current state",
        ),
        SimulationKind::StateBased => (
            "local model behavior on the recorded state",
            "EVM bytecode behavior or inclusion",
        ),
        SimulationKind::EthCall | SimulationKind::ContractCall => (
            "EVM call behavior at the pinned provider state",
            "future inclusion, ordering, or provider correctness",
        ),
        SimulationKind::Fork => (
            "transaction behavior on the pinned fork state",
            "future ordering or live token behavior changes",
        ),
        SimulationKind::HistoricalReplay => (
            "deterministic decision behavior on recorded evidence",
            "counterfactual chain inclusion",
        ),
        SimulationKind::HypotheticalInclusion => (
            "modeled inclusion under explicit assumptions",
            "actual sequencer ordering or realized capital PnL",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> SimulationRequest {
        SimulationRequest {
            kind: SimulationKind::ContractCall,
            block_number: 100,
            block_hash: "block-100".to_string(),
            from_address: None,
            target_contract: "executor".to_string(),
            expected_contract_code_hash: "code-v1".to_string(),
            calldata_hash: "calldata".to_string(),
            value: Amount::ZERO,
            state_overrides_hash: None,
            provider_id: "primary".to_string(),
            requested_at_unix_ms: 1_000,
        }
    }

    fn response() -> SimulationResponse {
        SimulationResponse {
            block_number: 100,
            block_hash: "block-100".to_string(),
            contract_code_hash: "code-v1".to_string(),
            gas_estimate: Some(100),
            gas_used: Some(90),
            output: Some(Amount(1)),
            net_pnl: Some(SignedAmount(1)),
            revert_reason: None,
            provider_id: "primary".to_string(),
            completed_at_unix_ms: 1_010,
            latency_ns: 10,
            state_drift_bps: BasisPoints(0),
            provider_disagreement: false,
            token_behavior_known_safe: true,
        }
    }

    fn policy() -> SimulationPolicy {
        SimulationPolicy {
            max_age_ms: 100,
            max_state_drift_bps: BasisPoints(10),
        }
    }

    #[test]
    fn complete_pinned_evidence_passes_without_a_signer() {
        let evidence = verify(&request(), &response(), policy()).unwrap();
        assert_eq!(evidence.classification, SimulationClassification::Passed);
        assert!(evidence.from_address.is_none());
    }

    #[test]
    fn block_provider_and_contract_mismatch_fail_closed() {
        let mut wrong_block = response();
        wrong_block.block_number = 101;
        assert_eq!(
            verify(&request(), &wrong_block, policy()),
            Err(SimulationError::AmbiguousBlock)
        );
        let mut disagreement = response();
        disagreement.provider_disagreement = true;
        assert_eq!(
            verify(&request(), &disagreement, policy()),
            Err(SimulationError::ProviderDisagreement)
        );
        let mut wrong_code = response();
        wrong_code.contract_code_hash = "code-v2".to_string();
        assert_eq!(
            verify(&request(), &wrong_code, policy()),
            Err(SimulationError::ContractVersionMismatch)
        );
    }

    #[test]
    fn stale_unknown_or_reverted_simulation_never_passes() {
        let mut stale = response();
        stale.completed_at_unix_ms = 1_101;
        assert_eq!(
            verify(&request(), &stale, policy()),
            Err(SimulationError::StaleState)
        );
        let mut unsafe_token = response();
        unsafe_token.token_behavior_known_safe = false;
        assert_eq!(
            verify(&request(), &unsafe_token, policy()),
            Err(SimulationError::UnsafeToken)
        );
        let mut reverted = response();
        reverted.revert_reason = Some("revert".to_string());
        assert_eq!(
            verify(&request(), &reverted, policy()),
            Err(SimulationError::Reverted)
        );
    }
}
