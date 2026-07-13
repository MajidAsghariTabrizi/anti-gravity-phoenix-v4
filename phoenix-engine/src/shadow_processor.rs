use crate::domain::{Address, Amount, Direction, PoolId, RouteId, TokenAddress};
use crate::engine_input::{EngineClassification, EngineInput};
use crate::graph::{PoolEdge, PoolGraph, Route};
use crate::opportunity::{Opportunity, ShadowDisposition};
use crate::origin::{OriginClassification, OriginDetector, OriginEvent};
use async_trait::async_trait;
use rpc_gateway::shadow_state::RpcQualityEvidence;
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
    pub evaluations: Vec<EvaluatedOpportunity>,
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
            evaluations: Vec::new(),
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
            evaluations: Vec::new(),
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
            evaluations: Vec::new(),
            action: ProcessingAction::Terminate,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CandidateBatch {
    pub evaluations: Vec<EvaluatedOpportunity>,
    pub evidence: Value,
}

#[derive(Clone, Debug)]
pub struct EvaluatedOpportunity {
    pub opportunity: Opportunity,
    pub rpc_quality: Vec<RpcQualityEvidence>,
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
    pub state_targets: Vec<Address>,
    pub strategy: RuntimeStrategy,
}

#[derive(Clone, Debug)]
pub struct RuntimeStrategy {
    pub min_input_amount: Amount,
    pub max_input_amount: Amount,
    pub max_evaluations: usize,
    pub minimum_net_profit: Amount,
    pub flash_premium_bps: u16,
    pub minimum_slippage_bps: u16,
    pub protocol_fees: Amount,
    pub estimated_execution_gas: u64,
    pub l1_data_fee: Amount,
    pub contract_overhead: Amount,
    pub failed_attempt_gas_cost: Amount,
    pub failure_probability_bps: u16,
    pub stale_state_loss: Amount,
    pub stale_quote_probability_bps: u16,
    pub state_drift_reserve: Amount,
    pub latency_reserve: Amount,
    pub uncertainty_reserve: Amount,
    pub replacement_transaction_cost: Amount,
    pub probability_of_success_bps: u16,
    pub max_gas_price_wei: u128,
    pub max_quote_age_ms: u64,
    pub max_simulation_age_ms: u64,
    pub min_confidence_bps: u16,
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
                    evaluations: Vec::new(),
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
        let mut evaluations = Vec::new();
        let mut evaluation_evidence = Vec::new();
        for route in &routes {
            match self.evaluator.evaluate(input, &origin, route).await {
                Ok(batch) => {
                    evaluations.extend(batch.evaluations);
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

        if evaluations.is_empty() {
            return ProcessResult {
                classification: EngineClassification::CandidateRejected,
                detail_class: "no_profitable_candidate",
                candidate_count: routes.len(),
                decision_count: 0,
                evidence: json!({
                    "route_fingerprints": route_fingerprints,
                    "evaluations": evaluation_evidence
                }),
                evaluations,
                action: ProcessingAction::Ack,
            };
        }
        let accepted = evaluations
            .iter()
            .any(|value| value.opportunity.decision.disposition == ShadowDisposition::Accepted);
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
            decision_count: evaluations.len(),
            evidence: json!({
                "route_fingerprints": route_fingerprints,
                "evaluations": evaluation_evidence
            }),
            evaluations,
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
    strategy: StrategySpec,
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
    direction: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrategySpec {
    min_input_amount: String,
    max_input_amount: String,
    max_evaluations: usize,
    minimum_net_profit: String,
    flash_premium_bps: u16,
    minimum_slippage_bps: u16,
    protocol_fees: String,
    estimated_execution_gas: u64,
    l1_data_fee: String,
    contract_overhead: String,
    failed_attempt_gas_cost: String,
    failure_probability_bps: u16,
    stale_state_loss: String,
    stale_quote_probability_bps: u16,
    state_drift_reserve: String,
    latency_reserve: String,
    uncertainty_reserve: String,
    replacement_transaction_cost: String,
    probability_of_success_bps: u16,
    max_gas_price_wei: String,
    max_quote_age_ms: u64,
    max_simulation_age_ms: u64,
    min_confidence_bps: u16,
}

impl RouteSpec {
    fn into_runtime(self) -> Result<RuntimeRoute, RouteRegistryError> {
        let RouteSpec {
            route_id,
            route_fingerprint,
            trigger_pool_id,
            legs,
            strategy,
        } = self;
        if !bounded(&route_id, 1, 128)
            || !bounded(&route_fingerprint, 1, 256)
            || !bounded(&trigger_pool_id, 1, 256)
            || legs.len() != 2
        {
            return Err(RouteRegistryError::InvalidRoute);
        }
        let (legs, state_targets): (Vec<_>, Vec<_>) = legs
            .into_iter()
            .map(RouteLegSpec::into_parts)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .unzip();
        if legs[0].pool_id.0 != trigger_pool_id
            || legs[0].protocol != "UniswapV3"
            || legs.iter().any(|leg| !leg.protocol.ends_with("V3"))
            || legs[0].token_out != legs[1].token_in
            || legs[1].token_out != legs[0].token_in
            || legs[0].pool_id == legs[1].pool_id
            || state_targets[0] == state_targets[1]
        {
            return Err(RouteRegistryError::InvalidRoute);
        }
        Ok(RuntimeRoute {
            route: Route {
                route_id: RouteId(route_id),
                legs,
            },
            fingerprint: route_fingerprint,
            state_targets,
            strategy: strategy.into_runtime()?,
        })
    }
}

impl RouteLegSpec {
    fn into_parts(self) -> Result<(PoolEdge, Address), RouteRegistryError> {
        if !bounded(&self.pool_id, 1, 256)
            || !bounded(&self.protocol, 1, 64)
            || self.fee == 0
            || self.fee >= 1_000_000
        {
            return Err(RouteRegistryError::InvalidRoute);
        }
        let token_in = Address::parse(&self.token_in)
            .map(TokenAddress)
            .map_err(|_| RouteRegistryError::InvalidRoute)?;
        let token_out = Address::parse(&self.token_out)
            .map(TokenAddress)
            .map_err(|_| RouteRegistryError::InvalidRoute)?;
        let state_target =
            Address::parse(&self.state_target).map_err(|_| RouteRegistryError::InvalidRoute)?;
        let direction = match self.direction.as_str() {
            "zero_for_one" => Direction::ZeroForOne,
            "one_for_zero" => Direction::OneForZero,
            _ => return Err(RouteRegistryError::InvalidRoute),
        };
        let direction_matches_token_order = match direction {
            Direction::ZeroForOne => token_in.0.as_str() < token_out.0.as_str(),
            Direction::OneForZero => token_out.0.as_str() < token_in.0.as_str(),
        };
        if token_in == token_out || !direction_matches_token_order {
            return Err(RouteRegistryError::InvalidRoute);
        }
        Ok((
            PoolEdge {
                pool_id: PoolId(self.pool_id),
                protocol: self.protocol,
                fee: self.fee,
                token_in,
                token_out,
                direction,
            },
            state_target,
        ))
    }
}

impl StrategySpec {
    fn into_runtime(self) -> Result<RuntimeStrategy, RouteRegistryError> {
        let strategy = RuntimeStrategy {
            min_input_amount: parse_amount(&self.min_input_amount)?,
            max_input_amount: parse_amount(&self.max_input_amount)?,
            max_evaluations: self.max_evaluations,
            minimum_net_profit: parse_amount(&self.minimum_net_profit)?,
            flash_premium_bps: self.flash_premium_bps,
            minimum_slippage_bps: self.minimum_slippage_bps,
            protocol_fees: parse_amount(&self.protocol_fees)?,
            estimated_execution_gas: self.estimated_execution_gas,
            l1_data_fee: parse_amount(&self.l1_data_fee)?,
            contract_overhead: parse_amount(&self.contract_overhead)?,
            failed_attempt_gas_cost: parse_amount(&self.failed_attempt_gas_cost)?,
            failure_probability_bps: self.failure_probability_bps,
            stale_state_loss: parse_amount(&self.stale_state_loss)?,
            stale_quote_probability_bps: self.stale_quote_probability_bps,
            state_drift_reserve: parse_amount(&self.state_drift_reserve)?,
            latency_reserve: parse_amount(&self.latency_reserve)?,
            uncertainty_reserve: parse_amount(&self.uncertainty_reserve)?,
            replacement_transaction_cost: parse_amount(&self.replacement_transaction_cost)?,
            probability_of_success_bps: self.probability_of_success_bps,
            max_gas_price_wei: parse_u128(&self.max_gas_price_wei)?,
            max_quote_age_ms: self.max_quote_age_ms,
            max_simulation_age_ms: self.max_simulation_age_ms,
            min_confidence_bps: self.min_confidence_bps,
        };
        if strategy.min_input_amount.0 == 0
            || strategy.max_input_amount < strategy.min_input_amount
            || strategy.max_evaluations == 0
            || strategy.max_evaluations > 64
            || strategy.minimum_net_profit.0 == 0
            || strategy.estimated_execution_gas == 0
            || strategy.max_gas_price_wei == 0
            || strategy.max_quote_age_ms == 0
            || strategy.max_simulation_age_ms == 0
            || strategy.probability_of_success_bps == 0
            || [
                strategy.flash_premium_bps,
                strategy.minimum_slippage_bps,
                strategy.failure_probability_bps,
                strategy.stale_quote_probability_bps,
                strategy.probability_of_success_bps,
                strategy.min_confidence_bps,
            ]
            .into_iter()
            .any(|value| value > 10_000)
        {
            return Err(RouteRegistryError::InvalidRoute);
        }
        Ok(strategy)
    }
}

fn parse_amount(value: &str) -> Result<Amount, RouteRegistryError> {
    let value = parse_u128(value)?;
    if value > i128::MAX as u128 {
        return Err(RouteRegistryError::InvalidRoute);
    }
    Ok(Amount(value))
}

fn parse_u128(value: &str) -> Result<u128, RouteRegistryError> {
    if value.is_empty()
        || value.len() > 39
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err(RouteRegistryError::InvalidRoute);
    }
    value
        .parse::<u128>()
        .map_err(|_| RouteRegistryError::InvalidRoute)
}

fn bounded(value: &str, minimum: usize, maximum: usize) -> bool {
    value.len() >= minimum && value.len() <= maximum && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ChainId, SequenceNumber, TxHash};
    use crate::messaging::NormalizedTx;
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
                    {{"pool_id":"{WETH}:{USDC}:500","state_target":"0x0000000000000000000000000000000000001001","protocol":"UniswapV3","fee":500,"token_in":"{WETH}","token_out":"{USDC}","direction":"zero_for_one"}},
                    {{"pool_id":"comparison-pool","state_target":"0x0000000000000000000000000000000000002001","protocol":"SushiSwapV3","fee":500,"token_in":"{USDC}","token_out":"{WETH}","direction":"one_for_zero"}}
                ],
                "strategy":{{
                    "min_input_amount":"100","max_input_amount":"1000","max_evaluations":16,
                    "minimum_net_profit":"1","flash_premium_bps":5,"minimum_slippage_bps":10,
                    "protocol_fees":"0","estimated_execution_gas":500000,"l1_data_fee":"1",
                    "contract_overhead":"1","failed_attempt_gas_cost":"1","failure_probability_bps":500,
                    "stale_state_loss":"1","stale_quote_probability_bps":100,"state_drift_reserve":"1",
                    "latency_reserve":"1","uncertainty_reserve":"1","replacement_transaction_cost":"1",
                    "probability_of_success_bps":8000,"max_gas_price_wei":"1000000000000",
                    "max_quote_age_ms":2000,"max_simulation_age_ms":2000,"min_confidence_bps":9000
                }}
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
        let wrong_token_order = route_json().replacen(
            "\"direction\":\"zero_for_one\"",
            "\"direction\":\"one_for_zero\"",
            1,
        );
        assert!(matches!(
            RouteRegistry::from_json(&wrong_token_order),
            Err(RouteRegistryError::InvalidRoute)
        ));
        let invalid_fee = route_json().replacen("\"fee\":500", "\"fee\":1000000", 1);
        assert!(matches!(
            RouteRegistry::from_json(&invalid_fee),
            Err(RouteRegistryError::InvalidRoute)
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
                evaluations: Vec::<EvaluatedOpportunity>::new(),
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
