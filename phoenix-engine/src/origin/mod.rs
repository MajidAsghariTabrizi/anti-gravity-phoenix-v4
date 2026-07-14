mod uniswap;

use crate::domain::{Address, Amount, PoolId, SequenceNumber, TokenAddress, TxHash};
use crate::messaging::NormalizedTx;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fmt;

pub const LEGACY_SWAP_ROUTER_ADDRESS: &str = "0xe592427a0aece92de3edee1f18e0157c05861564";
pub const SWAP_ROUTER_02_ADDRESS: &str = "0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45";
pub const UNIVERSAL_ROUTER_ADDRESS: &str = "0xa51afafe0263b40edaef0df8781ea9aa03e381a3";
pub const REVIEWED_ROUTER_ADDRESSES: [&str; 3] = [
    LEGACY_SWAP_ROUTER_ADDRESS,
    SWAP_ROUTER_02_ADDRESS,
    UNIVERSAL_ROUTER_ADDRESS,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RouterKind {
    LegacySwapRouter,
    SwapRouter02,
    UniversalRouter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OuterSelectorKind {
    LegacyExactInputSingle,
    LegacyExactInput,
    LegacyExactOutputSingle,
    LegacyMulticall,
    SwapRouter02ExactInputSingle,
    UniversalExecute,
    UniversalExecuteWithDeadline,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WrapperKind {
    Direct,
    Multicall,
    UniversalRouter,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecodedSwapKind {
    V3ExactInputSingle,
    V3ExactInput,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedReason {
    None,
    ExactOutput,
    AmbiguousMultiSwap,
    UnknownSelector,
    UnknownCommand,
    UnsupportedSwapFamily,
    NestedSubPlan,
    OptionalSwap,
    MissingSwap,
    MalformedCalldata,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OriginMetricKind {
    SupportedDirectV3,
    SupportedMulticall,
    SupportedUniversalRouterV3ExactIn,
    UnsupportedExactOutput,
    AmbiguousMultiSwap,
    MalformedRouterCalldata,
    UnknownOfficialRouterCommand,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OriginEvidence {
    pub router_kind: Option<RouterKind>,
    pub outer_selector_kind: OuterSelectorKind,
    pub wrapper_kind: WrapperKind,
    pub decoded_swap_kind: DecodedSwapKind,
    pub command_count: usize,
    pub v3_hop_count: usize,
    pub exact_in: Option<bool>,
    pub supported: bool,
    pub unsupported_reason: UnsupportedReason,
}

impl OriginEvidence {
    pub(crate) fn new(
        router_kind: RouterKind,
        outer_selector_kind: OuterSelectorKind,
        wrapper_kind: WrapperKind,
    ) -> Self {
        Self {
            router_kind: Some(router_kind),
            outer_selector_kind,
            wrapper_kind,
            decoded_swap_kind: DecodedSwapKind::None,
            command_count: 1,
            v3_hop_count: 0,
            exact_in: None,
            supported: false,
            unsupported_reason: UnsupportedReason::None,
        }
    }

    pub fn metric_kind(&self) -> OriginMetricKind {
        if self.supported {
            return match self.wrapper_kind {
                WrapperKind::Direct => OriginMetricKind::SupportedDirectV3,
                WrapperKind::Multicall => OriginMetricKind::SupportedMulticall,
                WrapperKind::UniversalRouter => OriginMetricKind::SupportedUniversalRouterV3ExactIn,
                WrapperKind::None => OriginMetricKind::MalformedRouterCalldata,
            };
        }
        match self.unsupported_reason {
            UnsupportedReason::ExactOutput => OriginMetricKind::UnsupportedExactOutput,
            UnsupportedReason::AmbiguousMultiSwap => OriginMetricKind::AmbiguousMultiSwap,
            UnsupportedReason::MalformedCalldata => OriginMetricKind::MalformedRouterCalldata,
            UnsupportedReason::None
            | UnsupportedReason::UnknownSelector
            | UnsupportedReason::UnknownCommand
            | UnsupportedReason::UnsupportedSwapFamily
            | UnsupportedReason::NestedSubPlan
            | UnsupportedReason::OptionalSwap
            | UnsupportedReason::MissingSwap => OriginMetricKind::UnknownOfficialRouterCommand,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OriginClassification {
    SupportedSwapOrigin(OriginEvent),
    KnownRouterUnsupportedCommand(OriginEvidence),
    PossibleAggregator,
    Irrelevant,
    Malformed(OriginEvidence),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OriginEvent {
    pub origin_tx_hash: TxHash,
    pub origin_sequence: SequenceNumber,
    pub router: Address,
    pub decoded_commands: Vec<String>,
    pub swap_path: Vec<TokenAddress>,
    pub exact_in: bool,
    pub amount: Amount,
    pub candidate_touched_pools: Vec<PoolId>,
    pub classification_evidence: OriginEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OriginConfigurationError {
    Empty,
    TooManyRouters,
    UnknownRouter,
    DuplicateRouter,
}

impl fmt::Display for OriginConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "the reviewed router registry is empty",
            Self::TooManyRouters => "the reviewed router registry is too large",
            Self::UnknownRouter => "the router is not a reviewed official entrypoint",
            Self::DuplicateRouter => "the reviewed router registry contains a duplicate",
        })
    }
}

impl std::error::Error for OriginConfigurationError {}

#[derive(Clone, Debug)]
pub struct OriginDetector {
    routers: HashMap<Address, RouterKind>,
}

impl OriginDetector {
    pub fn new(routers: Vec<Address>) -> Result<Self, OriginConfigurationError> {
        if routers.is_empty() {
            return Err(OriginConfigurationError::Empty);
        }
        if routers.len() > REVIEWED_ROUTER_ADDRESSES.len() {
            return Err(OriginConfigurationError::TooManyRouters);
        }
        let mut seen = HashSet::new();
        let mut reviewed = HashMap::new();
        for router in routers {
            if !seen.insert(router.clone()) {
                return Err(OriginConfigurationError::DuplicateRouter);
            }
            let kind =
                reviewed_router_kind(&router).ok_or(OriginConfigurationError::UnknownRouter)?;
            reviewed.insert(router, kind);
        }
        Ok(Self { routers: reviewed })
    }

    pub fn classify(&self, tx: &NormalizedTx) -> OriginClassification {
        let Some(to) = &tx.to else {
            return OriginClassification::Irrelevant;
        };
        let Some(router_kind) = self.routers.get(to).copied() else {
            return if tx.calldata.len() > 10 {
                OriginClassification::PossibleAggregator
            } else {
                OriginClassification::Irrelevant
            };
        };

        match uniswap::classify(router_kind, &tx.calldata) {
            uniswap::DecodeOutcome::Supported(decoded) => {
                OriginClassification::SupportedSwapOrigin(OriginEvent {
                    origin_tx_hash: tx.tx_hash.clone(),
                    origin_sequence: tx.sequence,
                    router: to.clone(),
                    decoded_commands: decoded.decoded_commands,
                    swap_path: decoded.swap_path.into_iter().map(TokenAddress).collect(),
                    exact_in: true,
                    amount: decoded.amount_in,
                    candidate_touched_pools: decoded.touched_pools,
                    classification_evidence: decoded.evidence,
                })
            }
            uniswap::DecodeOutcome::Unsupported(evidence) => {
                OriginClassification::KnownRouterUnsupportedCommand(evidence)
            }
            uniswap::DecodeOutcome::Malformed(evidence) => {
                OriginClassification::Malformed(evidence)
            }
        }
    }
}

pub fn reviewed_router_kind(address: &Address) -> Option<RouterKind> {
    match address.as_str() {
        LEGACY_SWAP_ROUTER_ADDRESS => Some(RouterKind::LegacySwapRouter),
        SWAP_ROUTER_02_ADDRESS => Some(RouterKind::SwapRouter02),
        UNIVERSAL_ROUTER_ADDRESS => Some(RouterKind::UniversalRouter),
        _ => None,
    }
}

pub(crate) struct DecodedSwap {
    pub decoded_commands: Vec<String>,
    pub swap_path: Vec<Address>,
    pub amount_in: Amount,
    pub touched_pools: Vec<PoolId>,
    pub evidence: OriginEvidence,
}
