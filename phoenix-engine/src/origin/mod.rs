use crate::domain::{Address, Amount, PoolId, SequenceNumber, TokenAddress, TxHash};
use crate::messaging::NormalizedTx;
pub use money_path_classifier::{
    DecodedSwapKind, OriginEvidence, OriginMetricKind, OuterSelectorKind, RouterKind,
    UnsupportedReason, WrapperKind, LEGACY_SWAP_ROUTER_ADDRESS, REVIEWED_ROUTER_ADDRESSES,
    SWAP_ROUTER_02_ADDRESS, UNIVERSAL_ROUTER_ADDRESS,
};
use std::collections::{HashMap, HashSet};
use std::fmt;

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

        match money_path_classifier::classify_router(router_kind, &tx.calldata) {
            money_path_classifier::DecodeOutcome::Supported(decoded) => {
                OriginClassification::SupportedSwapOrigin(OriginEvent {
                    origin_tx_hash: tx.tx_hash.clone(),
                    origin_sequence: tx.sequence,
                    router: to.clone(),
                    decoded_commands: decoded.decoded_commands,
                    swap_path: decoded
                        .swap_path
                        .into_iter()
                        .map(|address| {
                            Address::parse(address.as_str())
                                .map(TokenAddress)
                                .expect("shared decoder returns canonical addresses")
                        })
                        .collect(),
                    exact_in: true,
                    amount: Amount(decoded.amount_in.0),
                    candidate_touched_pools: decoded
                        .touched_pools
                        .into_iter()
                        .map(|pool| PoolId(pool.0))
                        .collect(),
                    classification_evidence: decoded.evidence,
                })
            }
            money_path_classifier::DecodeOutcome::Unsupported(evidence) => {
                OriginClassification::KnownRouterUnsupportedCommand(evidence)
            }
            money_path_classifier::DecodeOutcome::Malformed(evidence) => {
                OriginClassification::Malformed(evidence)
            }
        }
    }
}

pub fn reviewed_router_kind(address: &Address) -> Option<RouterKind> {
    money_path_classifier::reviewed_router_kind(address.as_str())
}
