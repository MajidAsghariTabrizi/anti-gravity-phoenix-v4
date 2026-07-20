mod uniswap;

pub mod domain;

use domain::{Address, Amount, PoolId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use thiserror::Error;

const ARBITRUM_ONE_CHAIN_ID: u64 = 42161;
const MAX_ROUTE_CONFIG_BYTES: usize = 64 * 1024;
const MAX_ROUTES: usize = 256;

pub const ADMISSION_POLICY_VERSION: &str = "money_path_v1";
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

impl RouterKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LegacySwapRouter => "legacy_swap_router",
            Self::SwapRouter02 => "swap_router02",
            Self::UniversalRouter => "universal_router",
        }
    }
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

impl OuterSelectorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LegacyExactInputSingle => "legacy_exact_input_single",
            Self::LegacyExactInput => "legacy_exact_input",
            Self::LegacyExactOutputSingle => "legacy_exact_output_single",
            Self::LegacyMulticall => "legacy_multicall",
            Self::SwapRouter02ExactInputSingle => "swap_router02_exact_input_single",
            Self::UniversalExecute => "universal_execute",
            Self::UniversalExecuteWithDeadline => "universal_execute_with_deadline",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WrapperKind {
    Direct,
    Multicall,
    UniversalRouter,
    None,
}

impl WrapperKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Multicall => "multicall",
            Self::UniversalRouter => "universal_router",
            Self::None => "none",
        }
    }
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
pub struct DecodedSwap {
    pub decoded_commands: Vec<String>,
    pub swap_path: Vec<Address>,
    pub amount_in: Amount,
    pub touched_pools: Vec<PoolId>,
    pub evidence: OriginEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeOutcome {
    Supported(DecodedSwap),
    Unsupported(OriginEvidence),
    Malformed(OriginEvidence),
}

pub fn classify_router(router_kind: RouterKind, calldata: &str) -> DecodeOutcome {
    uniswap::classify(router_kind, calldata)
}

pub fn reviewed_router_kind(address: &str) -> Option<RouterKind> {
    match address {
        LEGACY_SWAP_ROUTER_ADDRESS => Some(RouterKind::LegacySwapRouter),
        SWAP_ROUTER_02_ADDRESS => Some(RouterKind::SwapRouter02),
        UNIVERSAL_ROUTER_ADDRESS => Some(RouterKind::UniversalRouter),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IngressClassification {
    Irrelevant,
    UnsupportedInteresting,
    RelevantRouteInput,
}

impl IngressClassification {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Irrelevant => "irrelevant",
            Self::UnsupportedInteresting => "unsupported_interesting",
            Self::RelevantRouteInput => "relevant_route_input",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SafeDecoderSummary {
    pub router_kind: Option<RouterKind>,
    pub outer_selector_kind: OuterSelectorKind,
    pub wrapper_kind: WrapperKind,
    pub decoded_swap_kind: DecodedSwapKind,
    pub unsupported_reason: UnsupportedReason,
    pub command_count: usize,
    pub v3_hop_count: usize,
    pub reviewed_pool_matches: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassificationResult {
    pub classification: IngressClassification,
    pub detail_class: &'static str,
    pub summary: SafeDecoderSummary,
}

#[derive(Clone, Debug)]
pub struct MoneyPathClassifier {
    routers: HashMap<Address, RouterKind>,
    reviewed_pools: HashSet<PoolId>,
}

impl MoneyPathClassifier {
    pub fn from_release(
        admission_policy_version: &str,
        router_addresses: &[String],
        route_registry_json: &str,
    ) -> Result<Self, ClassifierError> {
        if admission_policy_version != ADMISSION_POLICY_VERSION {
            return Err(ClassifierError::AdmissionPolicy);
        }
        if router_addresses.is_empty() || router_addresses.len() > REVIEWED_ROUTER_ADDRESSES.len() {
            return Err(ClassifierError::RouterRegistry);
        }
        let mut routers = HashMap::new();
        for raw in router_addresses {
            let address = Address::parse(raw).map_err(|_| ClassifierError::RouterRegistry)?;
            let kind =
                reviewed_router_kind(address.as_str()).ok_or(ClassifierError::RouterRegistry)?;
            if routers.insert(address, kind).is_some() {
                return Err(ClassifierError::RouterRegistry);
            }
        }
        let reviewed_pools = reviewed_pools(route_registry_json)?;
        if reviewed_pools.is_empty() {
            return Err(ClassifierError::RouteRegistry);
        }
        Ok(Self {
            routers,
            reviewed_pools,
        })
    }

    pub fn classify(
        &self,
        chain_id: u64,
        destination: Option<&str>,
        calldata: &str,
    ) -> Result<ClassificationResult, ClassifierError> {
        if chain_id != ARBITRUM_ONE_CHAIN_ID {
            return Err(ClassifierError::Invariant);
        }
        let Some(destination) = destination else {
            return Ok(classification(
                IngressClassification::Irrelevant,
                "empty_destination",
                None,
                0,
            ));
        };
        let destination = Address::parse(destination).map_err(|_| ClassifierError::Invariant)?;
        let Some(router_kind) = self.routers.get(&destination).copied() else {
            let kind = if calldata.len() > 10 {
                IngressClassification::UnsupportedInteresting
            } else {
                IngressClassification::Irrelevant
            };
            let detail = if kind == IngressClassification::UnsupportedInteresting {
                "possible_aggregator"
            } else {
                "irrelevant_origin"
            };
            return Ok(classification(kind, detail, None, 0));
        };

        match classify_router(router_kind, calldata) {
            DecodeOutcome::Supported(decoded) => {
                if decoded.touched_pools.is_empty() {
                    return Err(ClassifierError::Invariant);
                }
                let reviewed_pool_matches = decoded
                    .touched_pools
                    .iter()
                    .filter(|pool| self.reviewed_pools.contains(*pool))
                    .count();
                let kind = if reviewed_pool_matches > 0 {
                    IngressClassification::RelevantRouteInput
                } else {
                    IngressClassification::Irrelevant
                };
                let detail = if reviewed_pool_matches > 0 {
                    "reviewed_route_touched"
                } else {
                    "no_affected_reviewed_route"
                };
                Ok(classification(
                    kind,
                    detail,
                    Some(decoded.evidence),
                    reviewed_pool_matches,
                ))
            }
            DecodeOutcome::Unsupported(evidence) => {
                let detail = match evidence.unsupported_reason {
                    UnsupportedReason::ExactOutput => "known_router_unsupported_exact_output",
                    UnsupportedReason::AmbiguousMultiSwap => "known_router_ambiguous_multi_swap",
                    _ => "known_router_unsupported_command",
                };
                Ok(classification(
                    IngressClassification::UnsupportedInteresting,
                    detail,
                    Some(evidence),
                    0,
                ))
            }
            DecodeOutcome::Malformed(evidence) => Ok(classification(
                IngressClassification::UnsupportedInteresting,
                "malformed_origin_calldata",
                Some(evidence),
                0,
            )),
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ClassifierError {
    #[error("money-path admission policy is invalid")]
    AdmissionPolicy,
    #[error("reviewed router registry is invalid")]
    RouterRegistry,
    #[error("reviewed route registry is invalid")]
    RouteRegistry,
    #[error("money-path classifier invariant failed")]
    Invariant,
}

fn classification(
    classification: IngressClassification,
    detail_class: &'static str,
    evidence: Option<OriginEvidence>,
    reviewed_pool_matches: usize,
) -> ClassificationResult {
    let evidence = evidence.unwrap_or(OriginEvidence {
        router_kind: None,
        outer_selector_kind: OuterSelectorKind::Unknown,
        wrapper_kind: WrapperKind::None,
        decoded_swap_kind: DecodedSwapKind::None,
        command_count: 0,
        v3_hop_count: 0,
        exact_in: None,
        supported: false,
        unsupported_reason: UnsupportedReason::None,
    });
    ClassificationResult {
        classification,
        detail_class,
        summary: SafeDecoderSummary {
            router_kind: evidence.router_kind,
            outer_selector_kind: evidence.outer_selector_kind,
            wrapper_kind: evidence.wrapper_kind,
            decoded_swap_kind: evidence.decoded_swap_kind,
            unsupported_reason: evidence.unsupported_reason,
            command_count: evidence.command_count,
            v3_hop_count: evidence.v3_hop_count,
            reviewed_pool_matches,
        },
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteSpec {
    route_id: String,
    route_fingerprint: String,
    trigger_pool_id: String,
    settlement_asset: String,
    settlement_asset_decimals: u8,
    legs: Vec<RouteLegSpec>,
    #[serde(rename = "strategy")]
    _strategy: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteLegSpec {
    pool_id: String,
    state_target: String,
    protocol: String,
    fee: u32,
    token_in: String,
    token_out: String,
    token_in_decimals: u8,
    token_out_decimals: u8,
    tick_spacing: i32,
    direction: String,
}

fn reviewed_pools(raw: &str) -> Result<HashSet<PoolId>, ClassifierError> {
    if raw.len() > MAX_ROUTE_CONFIG_BYTES {
        return Err(ClassifierError::RouteRegistry);
    }
    let routes: Vec<RouteSpec> =
        serde_json::from_str(raw).map_err(|_| ClassifierError::RouteRegistry)?;
    if routes.is_empty() || routes.len() > MAX_ROUTES {
        return Err(ClassifierError::RouteRegistry);
    }
    let mut route_ids = HashSet::new();
    let mut fingerprints = HashSet::new();
    let mut pools = HashSet::new();
    for route in routes {
        if !bounded(&route.route_id, 1, 128)
            || !bounded(&route.route_fingerprint, 1, 256)
            || !route_ids.insert(route.route_id)
            || !fingerprints.insert(route.route_fingerprint)
            || route.legs.len() != 2
        {
            return Err(ClassifierError::RouteRegistry);
        }
        let first = validate_leg(&route.legs[0])?;
        let second = validate_leg(&route.legs[1])?;
        let settlement_asset =
            Address::parse(&route.settlement_asset).map_err(|_| ClassifierError::RouteRegistry)?;
        if route.trigger_pool_id != route.legs[0].pool_id
            || first.1 != second.0
            || second.1 != first.0
            || route.legs[0].pool_id == route.legs[1].pool_id
            || route.legs[0].state_target == route.legs[1].state_target
            || settlement_asset != first.0
            || settlement_asset != second.1
            || !(1..=36).contains(&route.settlement_asset_decimals)
            || route.settlement_asset_decimals != route.legs[0].token_in_decimals
            || route.settlement_asset_decimals != route.legs[1].token_out_decimals
            || route.legs[0].token_out_decimals != route.legs[1].token_in_decimals
        {
            return Err(ClassifierError::RouteRegistry);
        }
        pools.insert(PoolId(route.legs[0].pool_id.clone()));
        pools.insert(PoolId(route.legs[1].pool_id.clone()));
    }
    Ok(pools)
}

fn validate_leg(leg: &RouteLegSpec) -> Result<(Address, Address), ClassifierError> {
    let token_in = Address::parse(&leg.token_in).map_err(|_| ClassifierError::RouteRegistry)?;
    let token_out = Address::parse(&leg.token_out).map_err(|_| ClassifierError::RouteRegistry)?;
    Address::parse(&leg.state_target).map_err(|_| ClassifierError::RouteRegistry)?;
    let expected_pool = canonical_pool_id(&token_in, &token_out, leg.fee);
    let direction_matches = match leg.direction.as_str() {
        "zero_for_one" => token_in.as_str() < token_out.as_str(),
        "one_for_zero" => token_out.as_str() < token_in.as_str(),
        _ => false,
    };
    if leg.protocol != "UniswapV3"
        || leg.fee == 0
        || leg.fee >= 1_000_000
        || token_in == token_out
        || !direction_matches
        || leg.pool_id != expected_pool.0
        || !(1..=36).contains(&leg.token_in_decimals)
        || !(1..=36).contains(&leg.token_out_decimals)
        || !(1..=887_272).contains(&leg.tick_spacing)
    {
        return Err(ClassifierError::RouteRegistry);
    }
    Ok((token_in, token_out))
}

fn canonical_pool_id(token_a: &Address, token_b: &Address, fee: u32) -> PoolId {
    let (token0, token1) = if token_a.as_str() < token_b.as_str() {
        (token_a, token_b)
    } else {
        (token_b, token_a)
    };
    PoolId(format!("{}:{}:{fee}", token0.as_str(), token1.as_str()))
}

fn bounded(value: &str, minimum: usize, maximum: usize) -> bool {
    value.len() >= minimum && value.len() <= maximum && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ethabi::ethereum_types::{H160, U256};
    use ethabi::{ParamType, Token};

    const ROUTES: &str = include_str!("../../fixtures/routes/weth_usdc_uniswap_v3.json");
    const WETH: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
    const USDC: &str = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";
    const DAI: &str = "0xda10009cbd5d07dd0cecc66161fc93d7c9000da1";

    fn classifier() -> MoneyPathClassifier {
        MoneyPathClassifier::from_release(
            ADMISSION_POLICY_VERSION,
            &REVIEWED_ROUTER_ADDRESSES
                .iter()
                .map(|value| (*value).to_string())
                .collect::<Vec<_>>(),
            ROUTES,
        )
        .unwrap()
    }

    fn address(value: &str) -> Token {
        Token::Address(H160::from_slice(
            &hex::decode(value.trim_start_matches("0x")).unwrap(),
        ))
    }

    fn router02_exact_input_single(token_in: &str, token_out: &str) -> String {
        let tuple = ParamType::Tuple(vec![
            ParamType::Address,
            ParamType::Address,
            ParamType::Uint(24),
            ParamType::Address,
            ParamType::Uint(256),
            ParamType::Uint(256),
            ParamType::Uint(160),
        ]);
        let mut bytes = ethabi::short_signature("exactInputSingle", &[tuple]).to_vec();
        bytes.extend(ethabi::encode(&[Token::Tuple(vec![
            address(token_in),
            address(token_out),
            Token::Uint(U256::from(500_u64)),
            address("0x1111111111111111111111111111111111111111"),
            Token::Uint(U256::from(1_000_000_u64)),
            Token::Uint(U256::from(1_u64)),
            Token::Uint(U256::zero()),
        ])]));
        format!("0x{}", hex::encode(bytes))
    }

    #[test]
    fn release_registry_is_strict_and_classifier_is_fail_closed() {
        let classifier = classifier();
        assert_eq!(
            classifier
                .classify(ARBITRUM_ONE_CHAIN_ID, None, "0x")
                .unwrap()
                .classification,
            IngressClassification::Irrelevant
        );
        assert_eq!(
            classifier.classify(1, None, "0x"),
            Err(ClassifierError::Invariant)
        );
        assert!(MoneyPathClassifier::from_release(
            ADMISSION_POLICY_VERSION,
            &[REVIEWED_ROUTER_ADDRESSES[0].to_string()],
            "[]"
        )
        .is_err());
        assert!(matches!(
            MoneyPathClassifier::from_release(
                "money_path_v2",
                &[REVIEWED_ROUTER_ADDRESSES[0].to_string()],
                ROUTES,
            ),
            Err(ClassifierError::AdmissionPolicy)
        ));
    }

    #[test]
    fn possible_aggregator_is_bounded_unsupported_evidence() {
        let classifier = classifier();
        let result = classifier
            .classify(
                ARBITRUM_ONE_CHAIN_ID,
                Some("0x1111111111111111111111111111111111111111"),
                "0x1234567890",
            )
            .unwrap();
        assert_eq!(
            result.classification,
            IngressClassification::UnsupportedInteresting
        );
        assert_eq!(result.detail_class, "possible_aggregator");
        let encoded = serde_json::to_string(&result.summary).unwrap();
        for forbidden in ["0x1111", "postgres://", "http://", "raw_tx", "calldata"] {
            assert!(!encoded.contains(forbidden));
        }
    }

    #[test]
    fn exact_shared_decoder_and_reviewed_pool_intersection_drive_relevance() {
        let classifier = classifier();
        let relevant = classifier
            .classify(
                ARBITRUM_ONE_CHAIN_ID,
                Some(SWAP_ROUTER_02_ADDRESS),
                &router02_exact_input_single(WETH, USDC),
            )
            .unwrap();
        assert_eq!(
            relevant.classification,
            IngressClassification::RelevantRouteInput
        );
        assert_eq!(relevant.detail_class, "reviewed_route_touched");
        assert_eq!(relevant.summary.reviewed_pool_matches, 1);

        let unrelated = classifier
            .classify(
                ARBITRUM_ONE_CHAIN_ID,
                Some(SWAP_ROUTER_02_ADDRESS),
                &router02_exact_input_single(WETH, DAI),
            )
            .unwrap();
        assert_eq!(unrelated.classification, IngressClassification::Irrelevant);
        assert_eq!(unrelated.detail_class, "no_affected_reviewed_route");

        let exact_output = classifier
            .classify(
                ARBITRUM_ONE_CHAIN_ID,
                Some(LEGACY_SWAP_ROUTER_ADDRESS),
                "0xdb3e2198",
            )
            .unwrap();
        assert_eq!(
            exact_output.classification,
            IngressClassification::UnsupportedInteresting
        );
        assert_eq!(
            exact_output.detail_class,
            "known_router_unsupported_exact_output"
        );
    }

    #[test]
    fn admission_is_mode_independent_and_contains_no_runtime_mode_branch() {
        let classifier = classifier();
        let calldata = router02_exact_input_single(WETH, USDC);
        let baseline = classifier
            .classify(
                ARBITRUM_ONE_CHAIN_ID,
                Some(SWAP_ROUTER_02_ADDRESS),
                &calldata,
            )
            .unwrap();
        for _synthetic_mode in ["SHADOW", "SIMULATE", "LIVE"] {
            assert_eq!(
                classifier
                    .classify(
                        ARBITRUM_ONE_CHAIN_ID,
                        Some(SWAP_ROUTER_02_ADDRESS),
                        &calldata,
                    )
                    .unwrap(),
                baseline
            );
        }

        let source = include_str!("lib.rs");
        for forbidden in [
            ["PHOENIX_", "MODE"].concat(),
            ["LIVE_", "EXECUTION"].concat(),
            ["std::", "env::"].concat(),
        ] {
            assert!(!source.contains(&forbidden));
        }
    }

    #[test]
    fn admission_is_independent_of_strategy_economics() {
        let calldata = router02_exact_input_single(WETH, USDC);
        let expected = classifier()
            .classify(
                ARBITRUM_ONE_CHAIN_ID,
                Some(SWAP_ROUTER_02_ADDRESS),
                &calldata,
            )
            .unwrap();
        let mut routes: serde_json::Value = serde_json::from_str(ROUTES).unwrap();
        routes[0]["strategy"] = serde_json::json!({
            "opaque_to_structural_admission": true
        });
        let structural_classifier = MoneyPathClassifier::from_release(
            ADMISSION_POLICY_VERSION,
            &REVIEWED_ROUTER_ADDRESSES
                .iter()
                .map(|value| (*value).to_string())
                .collect::<Vec<_>>(),
            &serde_json::to_string(&routes).unwrap(),
        )
        .unwrap();
        assert_eq!(
            structural_classifier
                .classify(
                    ARBITRUM_ONE_CHAIN_ID,
                    Some(SWAP_ROUTER_02_ADDRESS),
                    &calldata,
                )
                .unwrap(),
            expected
        );

        let source = include_str!("lib.rs");
        for forbidden in [
            ["minimum_", "net_profit"].concat(),
            ["estimated_execution_", "gas"].concat(),
            ["max_gas_", "price_wei"].concat(),
            ["max_simulation_", "age_ms"].concat(),
        ] {
            assert!(!source.contains(&forbidden));
        }
    }
}
