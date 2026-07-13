use crate::domain::{Address, Direction, PoolId, RouteId, TokenAddress};
use crate::engine_input::{EngineClassification, EngineInput};
use crate::graph::{PoolEdge, PoolGraph, Route};
use crate::opportunity::{Opportunity, ShadowDisposition};
use crate::origin::{OriginClassification, OriginDetector, OriginEvent};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use thiserror::Error;

const MAX_ROUTE_CONFIG_BYTES: usize = 64 * 1024;
const MAX_ROUTES: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessingAction {
    Ack,
    Retry,
    Terminate,
}

#[derive(Clone, Debug)]
pub struct ProcessResult {
    pub classification: EngineClassification,
    pub detail_class: &'static str,
    pub candidate_count: usize,
    pub decision_count: usize,
    pub evidence: Value,
    pub opportunities: Vec<Opportunity>,
    pub action: ProcessingAction,
}

impl ProcessResult {
    pub fn no_route(detail_class: &'static str, evidence: Value) -> Self {
        Self {
            classification: EngineClassification::NoRelevantRoute,
            detail_class,
            candidate_count: 0,
            decision_count: 0,
            evidence,
            opportunities: Vec::new(),
            action: ProcessingAction::Ack,
        }
    }

    pub fn transient(detail_class: &'static str, candidate_count: usize, evidence: Value) -> Self {
        Self {
            classification: EngineClassification::TransientDependencyFailure,
            detail_class,
            candidate_count,
            decision_count: 0,
            evidence,
            opportunities: Vec::new(),
            action: ProcessingAction::Retry,
        }
    }

    pub fn terminal(detail_class: &'static str, candidate_count: usize, evidence: Value) -> Self {
        Self {
            classification: EngineClassification::TerminalIntegrityFailure,
            detail_class,
            candidate_count,
            decision_count: 0,
            evidence,
            opportunities: Vec::new(),
            action: ProcessingAction::Terminate,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CandidateBatch {
    pub opportunities: Vec<Opportunity>,
    pub evidence: Value,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum EvaluationError {
    #[error("SHADOW candidate dependency is temporarily unavailable")]
    Transient(&'static str),
    #[error("SHADOW candidate evidence failed integrity validation")]
    Terminal(&'static str),
}

#[async_trait]
pub trait CandidateEvaluator: Send + Sync {
    async fn evaluate(
        &self,
        input: &EngineInput,
        origin: &OriginEvent,
        route: &RuntimeRoute,
    ) -> Result<CandidateBatch, EvaluationError>;
}

#[derive(Clone, Debug, Default)]
pub struct UnavailableEvaluator;

#[async_trait]
impl CandidateEvaluator for UnavailableEvaluator {
    async fn evaluate(
        &self,
        _input: &EngineInput,
        _origin: &OriginEvent,
        _route: &RuntimeRoute,
    ) -> Result<CandidateBatch, EvaluationError> {
        Err(EvaluationError::Transient("rpc_gateway_unavailable"))
    }
}

#[derive(Clone, Debug)]
pub struct RuntimeRoute {
    pub route: Route,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Default)]
pub struct RouteRegistry {
    graph: PoolGraph,
    routes: HashMap<String, RuntimeRoute>,
}

impl RouteRegistry {
    pub fn from_json(raw: &str) -> Result<Self, RouteRegistryError> {
        if raw.len() > MAX_ROUTE_CONFIG_BYTES {
            return Err(RouteRegistryError::Oversized);
        }
        let specs: Vec<RouteSpec> =
            serde_json::from_str(raw).map_err(|_| RouteRegistryError::InvalidJson)?;
        if specs.len() > MAX_ROUTES {
            return Err(RouteRegistryError::TooManyRoutes);
        }
        let mut registry = Self::default();
        let mut fingerprints = HashSet::new();
        for spec in specs {
            let runtime_route = spec.into_runtime()?;
            let route_id = runtime_route.route.route_id.0.clone();
            if registry.routes.contains_key(&route_id)
                || !fingerprints.insert(runtime_route.fingerprint.clone())
            {
                return Err(RouteRegistryError::DuplicateRoute);
            }
            registry
                .graph
                .add_two_pool_cycle(runtime_route.route.clone());
            registry.routes.insert(route_id, runtime_route);
        }
        Ok(registry)
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    pub fn affected_routes(&self, touched_pools: &[PoolId]) -> Vec<RuntimeRoute> {
        let mut seen = HashSet::new();
        let mut routes = Vec::new();
        for pool in touched_pools {
            for route in self.graph.affected_routes(pool) {
                if seen.insert(route.route_id.0.clone()) {
                    if let Some(runtime) = self.routes.get(&route.route_id.0) {
                        routes.push(runtime.clone());
                    }
                }
            }
        }
        routes.sort_by(|left, right| left.route.route_id.0.cmp(&right.route.route_id.0));
        routes
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RouteRegistryError {
    #[error("SHADOW route registry JSON is invalid")]
    InvalidJson,
    #[error("SHADOW route registry is oversized")]
    Oversized,
    #[error("SHADOW route registry has too many routes")]
    TooManyRoutes,
    #[error("SHADOW route registry contains an invalid route")]
    InvalidRoute,
    #[error("SHADOW route registry contains a duplicate route")]
    DuplicateRoute,
}

#[derive(Clone)]
pub struct ShadowProcessor {
    detector: OriginDetector,
    routes: RouteRegistry,
    evaluator: Arc<dyn CandidateEvaluator>,
}

impl std::fmt::Debug for ShadowProcessor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShadowProcessor")
            .field("detector", &self.detector)
            .field("routes", &self.routes)
            .finish_non_exhaustive()
    }
}

impl ShadowProcessor {
    pub fn new(
        routers: Vec<Address>,
        routes: RouteRegistry,
        evaluator: Arc<dyn CandidateEvaluator>,
    ) -> Self {
        Self {
            detector: OriginDetector::new(routers),
            routes,
            evaluator,
        }
    }

    pub fn strategy_configured(&self) -> bool {
        !self.routes.is_empty()
    }

    pub async fn process(&self, input: &EngineInput) -> ProcessResult {
        let origin = match self.detector.classify(&input.normalized) {
            OriginClassification::SupportedSwapOrigin(origin) => origin,
            OriginClassification::KnownRouterUnsupportedCommand => {
                return ProcessResult::no_route(
                    "known_router_unsupported_command",
                    json!({"origin_classification": "known_router_unsupported_command"}),
                );
            }
            OriginClassification::PossibleAggregator => {
                return ProcessResult::no_route(
                    "possible_aggregator",
                    json!({"origin_classification": "possible_aggregator"}),
                );
            }
            OriginClassification::Irrelevant => {
                return ProcessResult::no_route(
                    "irrelevant_origin",
                    json!({"origin_classification": "irrelevant"}),
                );
            }
            OriginClassification::Malformed => {
                return ProcessResult {
                    classification: EngineClassification::MalformedInternalEvent,
                    detail_class: "malformed_origin_calldata",
                    candidate_count: 0,
                    decision_count: 0,
                    evidence: json!({"origin_classification": "malformed"}),
                    opportunities: Vec::new(),
                    action: ProcessingAction::Retry,
                };
            }
        };
        let routes = self.routes.affected_routes(&origin.candidate_touched_pools);
        if routes.is_empty() {
            return ProcessResult::no_route(
                "no_affected_two_pool_route",
                json!({
                    "origin_classification": "supported_swap_origin",
                    "touched_pool_count": origin.candidate_touched_pools.len()
                }),
            );
        }

        let route_fingerprints = routes
            .iter()
            .map(|route| route.fingerprint.clone())
            .collect::<Vec<_>>();
        let mut opportunities = Vec::new();
        let mut evaluation_evidence = Vec::new();
        for route in &routes {
            match self.evaluator.evaluate(input, &origin, route).await {
                Ok(batch) => {
                    opportunities.extend(batch.opportunities);
                    evaluation_evidence.push(batch.evidence);
                }
                Err(EvaluationError::Transient(class)) => {
                    return ProcessResult::transient(
                        class,
                        routes.len(),
                        json!({
                            "origin_classification": "supported_swap_origin",
                            "route_fingerprints": route_fingerprints,
                            "dependency_failure_class": class
                        }),
                    );
                }
                Err(EvaluationError::Terminal(class)) => {
                    return ProcessResult::terminal(
                        class,
                        routes.len(),
                        json!({
                            "origin_classification": "supported_swap_origin",
                            "route_fingerprints": route_fingerprints,
                            "integrity_failure_class": class
                        }),
                    );
                }
            }
        }

        if opportunities.is_empty() {
            return ProcessResult {
                classification: EngineClassification::CandidateRejected,
                detail_class: "no_profitable_candidate",
                candidate_count: routes.len(),
                decision_count: 0,
                evidence: json!({
                    "route_fingerprints": route_fingerprints,
                    "evaluations": evaluation_evidence
                }),
                opportunities,
                action: ProcessingAction::Ack,
            };
        }
        let accepted = opportunities
            .iter()
            .any(|opportunity| opportunity.decision.disposition == ShadowDisposition::Accepted);
        ProcessResult {
            classification: if accepted {
                EngineClassification::ShadowAccepted
            } else {
                EngineClassification::CandidateRejected
            },
            detail_class: if accepted {
                "shadow_policy_accepted"
            } else {
                "shadow_policy_rejected"
            },
            candidate_count: routes.len(),
            decision_count: opportunities.len(),
            evidence: json!({
                "route_fingerprints": route_fingerprints,
                "evaluations": evaluation_evidence
            }),
            opportunities,
            action: ProcessingAction::Ack,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteSpec {
    route_id: String,
    route_fingerprint: String,
    trigger_pool_id: String,
    legs: Vec<RouteLegSpec>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteLegSpec {
    pool_id: String,
    protocol: String,
    fee: u32,
    token_in: String,
    token_out: String,
    direction: String,
}

impl RouteSpec {
    fn into_runtime(self) -> Result<RuntimeRoute, RouteRegistryError> {
        if !bounded(&self.route_id, 1, 128)
            || !bounded(&self.route_fingerprint, 1, 256)
            || !bounded(&self.trigger_pool_id, 1, 256)
            || self.legs.len() != 2
        {
            return Err(RouteRegistryError::InvalidRoute);
        }
        let legs = self
            .legs
            .into_iter()
            .map(RouteLegSpec::into_edge)
            .collect::<Result<Vec<_>, _>>()?;
        if legs[0].pool_id.0 != self.trigger_pool_id
            || legs[0].protocol != "UniswapV3"
            || legs.iter().any(|leg| !leg.protocol.ends_with("V3"))
            || legs[0].token_out != legs[1].token_in
            || legs[1].token_out != legs[0].token_in
            || legs[0].pool_id == legs[1].pool_id
        {
            return Err(RouteRegistryError::InvalidRoute);
        }
        Ok(RuntimeRoute {
            route: Route {
                route_id: RouteId(self.route_id),
                legs,
            },
            fingerprint: self.route_fingerprint,
        })
    }
}

impl RouteLegSpec {
    fn into_edge(self) -> Result<PoolEdge, RouteRegistryError> {
        if !bounded(&self.pool_id, 1, 256)
            || !bounded(&self.protocol, 1, 64)
            || self.fee == 0
            || self.fee > 1_000_000
        {
            return Err(RouteRegistryError::InvalidRoute);
        }
        let token_in = Address::parse(&self.token_in)
            .map(TokenAddress)
            .map_err(|_| RouteRegistryError::InvalidRoute)?;
        let token_out = Address::parse(&self.token_out)
            .map(TokenAddress)
            .map_err(|_| RouteRegistryError::InvalidRoute)?;
        let direction = match self.direction.as_str() {
            "zero_for_one" => Direction::ZeroForOne,
            "one_for_zero" => Direction::OneForZero,
            _ => return Err(RouteRegistryError::InvalidRoute),
        };
        Ok(PoolEdge {
            pool_id: PoolId(self.pool_id),
            protocol: self.protocol,
            fee: self.fee,
            token_in,
            token_out,
            direction,
        })
    }
}

fn bounded(value: &str, minimum: usize, maximum: usize) -> bool {
    value.len() >= minimum && value.len() <= maximum && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ChainId, SequenceNumber, TxHash};
    use crate::messaging::NormalizedTx;
    use crate::opportunity::Opportunity;
    use std::sync::Mutex;

    const ROUTER: &str = "0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45";
    const WETH: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
    const USDC: &str = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";

    #[derive(Debug)]
    struct FakeEvaluator {
        result: Mutex<Option<Result<CandidateBatch, EvaluationError>>>,
    }

    #[async_trait]
    impl CandidateEvaluator for FakeEvaluator {
        async fn evaluate(
            &self,
            _input: &EngineInput,
            _origin: &OriginEvent,
            _route: &RuntimeRoute,
        ) -> Result<CandidateBatch, EvaluationError> {
            self.result.lock().unwrap().take().unwrap()
        }
    }

    fn route_json() -> String {
        format!(
            r#"[{{
                "route_id":"weth-usdc-two-pool",
                "route_fingerprint":"weth-usdc-two-pool-v1",
                "trigger_pool_id":"{WETH}:{USDC}:500",
                "legs":[
                    {{"pool_id":"{WETH}:{USDC}:500","protocol":"UniswapV3","fee":500,"token_in":"{WETH}","token_out":"{USDC}","direction":"zero_for_one"}},
                    {{"pool_id":"comparison-pool","protocol":"SushiSwapV3","fee":500,"token_in":"{USDC}","token_out":"{WETH}","direction":"one_for_zero"}}
                ]
            }}]"#
        )
    }

    fn slot_address(address: &str) -> String {
        format!(
            "000000000000000000000000{}",
            address.trim_start_matches("0x")
        )
    }

    fn slot_u(value: u128) -> String {
        format!("{value:064x}")
    }

    fn input(to: &str) -> EngineInput {
        let calldata = format!(
            "0x414bf389{}{}{}{}{}{}{}{}",
            slot_address(WETH),
            slot_address(USDC),
            slot_u(500),
            slot_address("0x1111111111111111111111111111111111111111"),
            slot_u(1000),
            slot_u(0),
            slot_u(0),
            slot_u(0)
        );
        EngineInput {
            identity: crate::engine_input::InputIdentity {
                source_event_identity: "event-1".to_string(),
                source_sequence: 1,
                tx_hash: "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_string(),
                chain_id: 42161,
            },
            normalized: NormalizedTx {
                sequence: SequenceNumber(1),
                tx_hash: TxHash(
                    "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                        .to_string(),
                ),
                tx_type: "0x02".to_string(),
                chain_id: ChainId(42161),
                from: Address::parse("0x1111111111111111111111111111111111111111").unwrap(),
                to: Some(Address::parse(to).unwrap()),
                nonce: 1,
                value: "0".to_string(),
                calldata,
                gas_limit: "300000".to_string(),
                max_fee_per_gas: "100".to_string(),
                max_priority_fee_per_gas: "1".to_string(),
            },
            observed_at_unix_ms: 1,
            ingested_at_unix_ns: 1,
            canonical_payload: json!({}),
        }
    }

    #[test]
    fn registry_requires_exact_two_pool_v3_cycle() {
        let registry = RouteRegistry::from_json(&route_json()).unwrap();
        assert!(!registry.is_empty());
        assert_eq!(
            registry
                .affected_routes(&[PoolId(format!("{WETH}:{USDC}:500"))])
                .len(),
            1
        );
        assert!(matches!(
            RouteRegistry::from_json("[{}]"),
            Err(RouteRegistryError::InvalidJson)
        ));
    }

    #[tokio::test]
    async fn irrelevant_input_has_explicit_no_route_classification() {
        let processor = ShadowProcessor::new(
            vec![Address::parse(ROUTER).unwrap()],
            RouteRegistry::from_json(&route_json()).unwrap(),
            Arc::new(UnavailableEvaluator),
        );
        let result = processor
            .process(&input("0x9999999999999999999999999999999999999999"))
            .await;
        assert_eq!(result.classification, EngineClassification::NoRelevantRoute);
        assert_eq!(result.action, ProcessingAction::Ack);
    }

    #[tokio::test]
    async fn supported_route_records_transient_dependency_instead_of_synthetic_profit() {
        let processor = ShadowProcessor::new(
            vec![Address::parse(ROUTER).unwrap()],
            RouteRegistry::from_json(&route_json()).unwrap(),
            Arc::new(UnavailableEvaluator),
        );
        let result = processor.process(&input(ROUTER)).await;
        assert_eq!(
            result.classification,
            EngineClassification::TransientDependencyFailure
        );
        assert_eq!(result.detail_class, "rpc_gateway_unavailable");
        assert_eq!(result.candidate_count, 1);
        assert_eq!(result.action, ProcessingAction::Retry);
    }

    #[tokio::test]
    async fn empty_real_evaluation_is_auditable_candidate_rejection() {
        let evaluator = FakeEvaluator {
            result: Mutex::new(Some(Ok(CandidateBatch {
                opportunities: Vec::<Opportunity>::new(),
                evidence: json!({"reason": "no_spread"}),
            }))),
        };
        let processor = ShadowProcessor::new(
            vec![Address::parse(ROUTER).unwrap()],
            RouteRegistry::from_json(&route_json()).unwrap(),
            Arc::new(evaluator),
        );
        let result = processor.process(&input(ROUTER)).await;
        assert_eq!(
            result.classification,
            EngineClassification::CandidateRejected
        );
        assert_eq!(result.action, ProcessingAction::Ack);
    }
}
