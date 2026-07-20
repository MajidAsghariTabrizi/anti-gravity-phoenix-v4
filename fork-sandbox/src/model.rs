use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const FORK_EVIDENCE_SCHEMA_VERSION: &str = "phoenix.fork-evidence.v1";
pub const PLAN_SCHEMA_VERSION: &str = "phoenix.unsigned-fork-plan.v1";
pub const RESULT_SCHEMA_VERSION: &str = "phoenix.fork-result.v1";
pub const ARBITRUM_ONE_CHAIN_ID: u64 = 42161;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedOpportunity {
    pub shadow_decision_id: String,
    pub source_event_identity: String,
    pub chain_id: u64,
    pub route_id: String,
    pub route_fingerprint: String,
    pub origin_router: String,
    pub token_path: Vec<String>,
    pub pool_path: Vec<String>,
    pub pool_address_path: Vec<String>,
    pub protocol_path: Vec<String>,
    pub direction_path: Vec<String>,
    pub fee_path: Vec<u32>,
    pub expected_leg_outputs: Vec<String>,
    pub pool_state_hash_path: Vec<String>,
    pub input_amount: String,
    pub expected_output: String,
    pub gross_profit: String,
    pub total_cost: String,
    pub expected_net_pnl: String,
    pub minimum_required_net_pnl: String,
    pub execution_gas: String,
    pub gas_price: String,
    pub detected_at: DateTime<Utc>,
    pub opportunity_expires_at: DateTime<Utc>,
    pub pinned_block_number: u64,
    pub pinned_block_hash: String,
    pub primary_state_hash: String,
    pub route_config_hash: String,
    pub primary_provider_id: String,
    pub secondary_provider_id: Option<String>,
    pub secondary_state_hash: Option<String>,
    pub secondary_block_number: Option<u64>,
    pub secondary_block_hash: Option<String>,
    pub secondary_route_config_hash: Option<String>,
    pub verification_status: String,
    pub independent_verification_status: String,
    pub agreement_state: String,
    pub model_version: String,
    pub policy_version: String,
    pub disposition: String,
    pub primary_rejection_reason: Option<String>,
    pub primary_profitability_status: String,
    pub evidence_completeness_status: String,
    pub fork_evidence_schema_version: String,
    pub shadow_only: bool,
    pub execution_eligible: bool,
    pub execution_request_created: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoutePlan {
    pub route_id: String,
    pub route_fingerprint: String,
    pub pool_ids: Vec<String>,
    pub pool_addresses: Vec<String>,
    pub protocols: Vec<String>,
    pub directions: Vec<String>,
    pub fees: Vec<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PinnedBlockEvidence {
    pub number: u64,
    pub hash: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerificationEvidence {
    pub verification_status: String,
    pub independent_verification_status: String,
    pub agreement_state: String,
    pub primary_provider_id: String,
    pub secondary_provider_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PredictedEconomics {
    pub gross_profit: String,
    pub total_cost: String,
    pub net_pnl: String,
    pub minimum_required_net_pnl: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UnsignedTransactionPlan {
    pub schema_version: String,
    pub shadow_decision_id: String,
    pub source_event_identity: String,
    pub chain_id: u64,
    pub route: RoutePlan,
    pub token_path: Vec<String>,
    pub origin_router: String,
    pub input_amount: String,
    pub maximum_input_amount: String,
    pub expected_output: String,
    pub expected_leg_outputs: Vec<String>,
    pub minimum_output: String,
    pub minimum_leg_outputs: Vec<String>,
    pub minimum_profit: String,
    pub calldata: String,
    pub calldata_hash: String,
    pub value: String,
    pub gas_estimate: u64,
    pub gas_price_wei: String,
    pub deadline: u64,
    pub target_contract: String,
    pub target_code_hash: String,
    pub simulation_from: String,
    pub pinned_block: PinnedBlockEvidence,
    pub route_hash: String,
    pub primary_state_hash: String,
    pub pool_state_hash_path: Vec<String>,
    pub verification: VerificationEvidence,
    pub predicted: PredictedEconomics,
    pub model_version: String,
    pub policy_version: String,
    pub unsigned: bool,
    pub fork_only: bool,
    pub shadow_only: bool,
    pub live_execution: bool,
    pub execution_eligible: bool,
    pub execution_request_created: bool,
    pub public_broadcast: bool,
    pub signer_used: bool,
}

impl UnsignedTransactionPlan {
    pub fn canonical_hash(&self) -> Result<String, serde_json::Error> {
        serde_json::to_vec(self).map(|encoded| hex::encode(Sha256::digest(encoded)))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SimulationStatus {
    Passed,
    Reverted,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ForkIdentity {
    pub chain_id: u64,
    pub fork_block: PinnedBlockEvidence,
    pub fork_instance_hash: String,
    pub local_block: PinnedBlockEvidence,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SimulationEvidence {
    pub rpc_methods: Vec<String>,
    pub target_code_hash: String,
    pub observed_pool_state_hashes: Vec<String>,
    pub observed_aggregate_state_hash: String,
    pub call_output_hash: Option<String>,
    pub trace_hash: Option<String>,
    pub settled_route_hash: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CounterfactualResultBody {
    pub schema_version: String,
    pub plan_hash: String,
    pub shadow_decision_id: String,
    pub status: SimulationStatus,
    pub predicted_gross_profit: String,
    pub predicted_total_cost: String,
    pub predicted_net_pnl: String,
    pub simulated_gross_profit: Option<String>,
    pub simulated_gas_cost: Option<String>,
    pub simulated_balance_delta: Option<String>,
    pub simulated_net_pnl: Option<String>,
    pub prediction_error: Option<String>,
    pub gas_estimate: Option<u64>,
    pub gas_used: Option<u64>,
    pub model_version: String,
    pub policy_version: String,
    pub fork: ForkIdentity,
    pub simulated_at: DateTime<Utc>,
    pub revert_reason: Option<String>,
    pub evidence: SimulationEvidence,
    pub fork_only: bool,
    pub shadow_only: bool,
    pub live_execution: bool,
    pub execution_eligible: bool,
    pub execution_request_created: bool,
    pub public_broadcast: bool,
    pub signer_used: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CounterfactualResult {
    pub result_hash: String,
    #[serde(flatten)]
    pub body: CounterfactualResultBody,
}

impl CounterfactualResult {
    pub fn from_body(body: CounterfactualResultBody) -> Result<Self, serde_json::Error> {
        let encoded = serde_json::to_vec(&body)?;
        Ok(Self {
            result_hash: hex::encode(Sha256::digest(encoded)),
            body,
        })
    }

    pub fn validate_plan_binding(
        &self,
        plan: &UnsignedTransactionPlan,
    ) -> Result<(), EvidenceIntegrityError> {
        let plan_hash = plan
            .canonical_hash()
            .map_err(|_| EvidenceIntegrityError::Invalid)?;
        let result_hash = Self::from_body(self.body.clone())
            .map_err(|_| EvidenceIntegrityError::Invalid)?
            .result_hash;
        let calldata = plan
            .calldata
            .strip_prefix("0x")
            .filter(|encoded| {
                !encoded.is_empty()
                    && encoded.len() % 2 == 0
                    && encoded
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .and_then(|encoded| hex::decode(encoded).ok())
            .ok_or(EvidenceIntegrityError::Invalid)?;
        if plan.schema_version != PLAN_SCHEMA_VERSION
            || self.body.schema_version != RESULT_SCHEMA_VERSION
            || self.body.plan_hash != plan_hash
            || self.result_hash != result_hash
            || hex::encode(Sha256::digest(calldata)) != plan.calldata_hash
            || self.body.shadow_decision_id != plan.shadow_decision_id
            || plan.chain_id != ARBITRUM_ONE_CHAIN_ID
            || self.body.fork.chain_id != plan.chain_id
            || self.body.fork.fork_block != plan.pinned_block
            || self.body.fork.local_block.number < plan.pinned_block.number
            || self.body.predicted_gross_profit != plan.predicted.gross_profit
            || self.body.predicted_total_cost != plan.predicted.total_cost
            || self.body.predicted_net_pnl != plan.predicted.net_pnl
            || self.body.model_version != plan.model_version
            || self.body.policy_version != plan.policy_version
            || self.body.evidence.target_code_hash != plan.target_code_hash
            || self.body.evidence.observed_pool_state_hashes != plan.pool_state_hash_path
            || self.body.evidence.observed_aggregate_state_hash != plan.primary_state_hash
            || !plan.unsigned
            || !plan.fork_only
            || !plan.shadow_only
            || plan.live_execution
            || plan.execution_eligible
            || plan.execution_request_created
            || plan.public_broadcast
            || plan.signer_used
            || !self.body.fork_only
            || !self.body.shadow_only
            || self.body.live_execution
            || self.body.execution_eligible
            || self.body.execution_request_created
            || self.body.public_broadcast
            || self.body.signer_used
        {
            return Err(EvidenceIntegrityError::Invalid);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum EvidenceIntegrityError {
    #[error("fork plan and simulation evidence are not canonically bound")]
    Invalid,
}
