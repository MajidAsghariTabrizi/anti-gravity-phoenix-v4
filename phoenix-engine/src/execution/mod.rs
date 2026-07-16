pub use crate::opportunity::Opportunity;

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
        assert_eq!(
            ExecutionMode::from_env("LIVE", false),
            ExecutionMode::Shadow
        );
        assert_eq!(ExecutionMode::from_env("LIVE", true), ExecutionMode::Live);
    }
}
