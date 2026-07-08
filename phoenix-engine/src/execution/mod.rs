use crate::domain::{Amount, OpportunityId, RouteId};
use crate::graph::PoolEdge;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionMode {
    Shadow,
    Simulate,
    Live,
}

impl ExecutionMode {
    pub fn from_env(mode: &str, live_execution: bool) -> Self {
        match mode.to_ascii_uppercase().as_str() {
            "LIVE" if live_execution => Self::Live,
            "SIMULATE" => Self::Simulate,
            _ => Self::Shadow,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Shadow => "SHADOW",
            Self::Simulate => "SIMULATE",
            Self::Live => "LIVE",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opportunity {
    pub opportunity_id: OpportunityId,
    pub route_id: RouteId,
    pub origin_tx_hash: String,
    pub origin_sequence: u64,
    pub snapshot_id: String,
    pub flash_asset: String,
    pub optimized_amount: Amount,
    pub expected_gross_profit: Amount,
    pub expected_flash_premium: Amount,
    pub expected_execution_cost: Amount,
    pub expected_net_profit: Amount,
    pub exact_ordered_legs: Vec<PoolEdge>,
    pub min_profit: Amount,
    pub expires_at_unix_ms: u64,
    pub created_at_monotonic_ns: u128,
    pub simulation_latency_ns: u128,
}

pub struct ExecutionCoordinator {
    mode: ExecutionMode,
}

impl ExecutionCoordinator {
    pub fn new(mode: ExecutionMode) -> Self {
        Self { mode }
    }

    pub fn mode(&self) -> ExecutionMode {
        self.mode
    }

    pub fn live_allowed(&self) -> bool {
        self.mode == ExecutionMode::Live
    }

    pub fn submit(&self, _opportunity: &Opportunity) -> ExecutionDecision {
        match self.mode {
            ExecutionMode::Shadow => ExecutionDecision::RecordedShadow,
            ExecutionMode::Simulate => ExecutionDecision::ColdSimulationOnly,
            ExecutionMode::Live => ExecutionDecision::RequiresSignerAndSequencerSubmit,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionDecision {
    RecordedShadow,
    ColdSimulationOnly,
    RequiresSignerAndSequencerSubmit,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_requires_explicit_flag() {
        assert_eq!(ExecutionMode::from_env("LIVE", false), ExecutionMode::Shadow);
        assert_eq!(ExecutionMode::from_env("LIVE", true), ExecutionMode::Live);
    }
}

