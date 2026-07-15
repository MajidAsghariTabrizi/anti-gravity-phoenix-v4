use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
}
