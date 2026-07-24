use crate::amm::v3::{
    amount_0_delta, amount_1_delta, amount_less_fee, div_rounding_up, next_sqrt_price_from_input,
    spot_output, sqrt_ratio_at_tick, u256_to_u128, u512_to_u128,
};
use crate::domain::{Amount, Direction, DomainError};
use chrono::{SecondsFormat, TimeZone, Utc};
use ethabi::{Address as EthAddress, Contract, Token};
use primitive_types::{U256, U512};
use rpc_gateway::hunter_state::{
    HunterStateError, PinnedV3PoolState, ProviderStateAgreement, PINNED_V3_STATE_SCHEMA,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::io::Cursor;
use thiserror::Error;

const ROUTE_UNIVERSE_SCHEMA: &str = "phoenix.route-universe.v1";
const ROUTE_POLICY_SCHEMA: &str = "phoenix.route-policy.v1";
const CANDIDATE_SCHEMA: &str = "phoenix.autonomous-candidate.v1";
const FEE_DENOMINATOR: u128 = 1_000_000;
const BPS_DENOMINATOR: u128 = 10_000;
const MAX_SEEN_EVENT_ROUTES: usize = 4_096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HunterBounds {
    pub maximum_assets: usize,
    pub maximum_pools: usize,
    pub maximum_routes: usize,
    pub maximum_cycles_per_settlement_asset: usize,
    pub maximum_routes_per_pool: usize,
    pub maximum_affected_routes_per_event: usize,
    pub maximum_tick_words_per_pool: usize,
    pub maximum_initialized_ticks: usize,
    pub maximum_tick_crossings_per_leg: u32,
    pub maximum_size_probes: usize,
    pub maximum_local_refinements: usize,
    pub maximum_concurrent_evaluations: usize,
    pub maximum_candidate_outputs_per_event: usize,
}

impl Default for HunterBounds {
    fn default() -> Self {
        Self {
            maximum_assets: 32,
            maximum_pools: 256,
            maximum_routes: 4_096,
            maximum_cycles_per_settlement_asset: 1_024,
            maximum_routes_per_pool: 1_024,
            maximum_affected_routes_per_event: 256,
            maximum_tick_words_per_pool: 32,
            maximum_initialized_ticks: 512,
            maximum_tick_crossings_per_leg: 64,
            maximum_size_probes: 32,
            maximum_local_refinements: 8,
            maximum_concurrent_evaluations: 16,
            maximum_candidate_outputs_per_event: 16,
        }
    }
}

impl HunterBounds {
    fn validate(self) -> Result<Self, HunterError> {
        let counts = [
            self.maximum_assets,
            self.maximum_pools,
            self.maximum_routes,
            self.maximum_cycles_per_settlement_asset,
            self.maximum_routes_per_pool,
            self.maximum_affected_routes_per_event,
            self.maximum_tick_words_per_pool,
            self.maximum_initialized_ticks,
            self.maximum_size_probes,
            self.maximum_concurrent_evaluations,
            self.maximum_candidate_outputs_per_event,
        ];
        if counts.contains(&0)
            || self.maximum_local_refinements > self.maximum_size_probes
            || self.maximum_tick_crossings_per_leg == 0
        {
            return Err(HunterError::InvalidBounds);
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct UniverseAsset {
    asset_id: String,
    address: String,
    symbol: String,
    decimals: u8,
    maximum_input_amount: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct UniverseRouter {
    router_id: String,
    protocol_id: String,
    address: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct UniverseFactory {
    factory_id: String,
    protocol_id: String,
    address: String,
    pool_init_code_hash: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct UniversePool {
    pool_id: String,
    protocol_id: String,
    factory_address: String,
    address: String,
    token0: String,
    token1: String,
    fee: u32,
    tick_spacing: i32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct UniverseHardCaps {
    global_maximum_input_amount: String,
    maximum_tick_crossings: u32,
    maximum_size_evaluations: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RouteUniverse {
    schema_version: String,
    universe_id: String,
    universe_version: u32,
    chain_id: u64,
    settlement_assets: Vec<UniverseAsset>,
    intermediate_assets: Vec<UniverseAsset>,
    routers: Vec<UniverseRouter>,
    factories: Vec<UniverseFactory>,
    pools: Vec<UniversePool>,
    maximum_route_legs: usize,
    maximum_total_routes: usize,
    maximum_routes_per_event: usize,
    default_hard_caps: UniverseHardCaps,
    universe_hash: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RoutePolicy {
    schema_version: String,
    policy_id: String,
    policy_version: u32,
    chain_id: u64,
    route_fingerprint: String,
    route_universe_hash: String,
    settlement_asset: String,
    token_path: Vec<String>,
    pool_addresses: Vec<String>,
    factory_addresses: Vec<String>,
    protocol_ids: Vec<String>,
    fees: Vec<u32>,
    directions: Vec<String>,
    minimum_input_amount: String,
    maximum_input_amount: String,
    minimum_retained_profit: String,
    maximum_price_impact_bps: u16,
    maximum_slippage_bps: u16,
    maximum_pool_utilization_bps: u16,
    maximum_tick_crossings: u32,
    maximum_state_age_blocks: u64,
    maximum_quote_age_ms: u64,
    maximum_candidate_age_ms: u64,
    allowed_submission_channels: Vec<String>,
    maximum_ordering_payment: String,
    per_transaction_maximum_loss: String,
    per_route_daily_loss: String,
    maximum_consecutive_losses: u32,
    minimum_observation_count: u64,
    minimum_recent_fork_pass_rate_bps: u16,
    maximum_prediction_error_bps: u16,
    enabled_for_shadow: bool,
    enabled_for_autonomous_live: bool,
    policy_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HunterRouteLeg {
    pub factory_address: String,
    pub pool_id: String,
    pub pool_address: String,
    pub protocol_id: String,
    pub token_in: String,
    pub token_out: String,
    pub fee: u32,
    pub tick_spacing: i32,
    pub direction: Direction,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EnumerableRoute {
    pub semantic_hash: String,
    pub settlement_asset: String,
    pub legs: Vec<HunterRouteLeg>,
}

#[derive(Clone, Debug)]
struct BoundRoute {
    route: EnumerableRoute,
    route_fingerprint: String,
    policy: RoutePolicy,
    settlement_maximum_input: u128,
    universe_maximum_input: u128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteGraphSummary {
    pub asset_count: usize,
    pub pool_count: usize,
    pub enumerable_route_count: usize,
    pub shadow_enabled_route_count: usize,
    pub routes_per_leg_count: BTreeMap<usize, usize>,
}

#[derive(Clone, Debug)]
pub struct HunterRouteGraph {
    universe_hash: String,
    routes: Vec<EnumerableRoute>,
    bound_routes: Vec<BoundRoute>,
    affected: BTreeMap<String, Vec<usize>>,
    summary: RouteGraphSummary,
}

impl HunterRouteGraph {
    pub fn from_contracts(
        universe_json: &str,
        policy_json_documents: &[&str],
        bounds: HunterBounds,
    ) -> Result<Self, HunterError> {
        let bounds = bounds.validate()?;
        let universe_value: Value =
            serde_json::from_str(universe_json).map_err(|_| HunterError::InvalidUniverse)?;
        verify_contract_hash(
            &universe_value,
            "universe_hash",
            "route-universe",
            ROUTE_UNIVERSE_SCHEMA,
        )?;
        let universe: RouteUniverse =
            serde_json::from_value(universe_value).map_err(|_| HunterError::InvalidUniverse)?;
        validate_universe(&universe, bounds)?;
        let mut policies = Vec::with_capacity(policy_json_documents.len());
        for raw in policy_json_documents {
            let value: Value = serde_json::from_str(raw).map_err(|_| HunterError::InvalidPolicy)?;
            verify_contract_hash(&value, "policy_hash", "route-policy", ROUTE_POLICY_SCHEMA)?;
            let policy: RoutePolicy =
                serde_json::from_value(value).map_err(|_| HunterError::InvalidPolicy)?;
            validate_policy(&policy, &universe)?;
            policies.push(policy);
        }
        policies.sort_by(|left, right| left.route_fingerprint.cmp(&right.route_fingerprint));
        if policies
            .windows(2)
            .any(|pair| pair[0].route_fingerprint == pair[1].route_fingerprint)
        {
            return Err(HunterError::DuplicateRoute);
        }

        let routes = enumerate_routes(&universe, bounds)?;
        let asset_limits = universe
            .settlement_assets
            .iter()
            .chain(universe.intermediate_assets.iter())
            .map(|asset| {
                parse_u128(&asset.maximum_input_amount).map(|limit| (asset.address.clone(), limit))
            })
            .collect::<Result<HashMap<_, _>, _>>()?;
        let universe_limit = parse_u128(&universe.default_hard_caps.global_maximum_input_amount)?;
        let mut matched_policies = HashSet::new();
        let mut bound_routes = Vec::new();
        for route in &routes {
            let matches = policies
                .iter()
                .enumerate()
                .filter(|(_, policy)| policy_matches_route(policy, route))
                .collect::<Vec<_>>();
            if matches.len() > 1 {
                return Err(HunterError::DuplicateRoute);
            }
            if let Some((index, policy)) = matches.first() {
                matched_policies.insert(*index);
                if policy.enabled_for_shadow {
                    let settlement_maximum_input = *asset_limits
                        .get(&route.settlement_asset)
                        .ok_or(HunterError::InvalidUniverse)?;
                    bound_routes.push(BoundRoute {
                        route: route.clone(),
                        route_fingerprint: policy.route_fingerprint.clone(),
                        policy: (*policy).clone(),
                        settlement_maximum_input,
                        universe_maximum_input: universe_limit,
                    });
                }
            }
        }
        if matched_policies.len() != policies.len() {
            return Err(HunterError::PolicyRouteMismatch);
        }
        bound_routes.sort_by(|left, right| left.route_fingerprint.cmp(&right.route_fingerprint));
        let affected = build_affected_index(&bound_routes, bounds)?;
        let mut routes_per_leg_count = BTreeMap::new();
        for route in &routes {
            *routes_per_leg_count.entry(route.legs.len()).or_insert(0) += 1;
        }
        let summary = RouteGraphSummary {
            asset_count: universe.settlement_assets.len() + universe.intermediate_assets.len(),
            pool_count: universe.pools.len(),
            enumerable_route_count: routes.len(),
            shadow_enabled_route_count: bound_routes.len(),
            routes_per_leg_count,
        };
        Ok(Self {
            universe_hash: universe.universe_hash,
            routes,
            bound_routes,
            affected,
            summary,
        })
    }

    pub fn summary(&self) -> &RouteGraphSummary {
        &self.summary
    }

    pub fn enumerable_routes(&self) -> &[EnumerableRoute] {
        &self.routes
    }

    fn affected_route_indices(
        &self,
        pool_addresses: &[String],
        maximum: usize,
    ) -> Result<Vec<usize>, HunterError> {
        let mut indices = BTreeSet::new();
        for pool in pool_addresses {
            if !canonical_address(pool) {
                return Err(HunterError::InvalidEvent);
            }
            if let Some(route_indices) = self.affected.get(pool) {
                indices.extend(route_indices.iter().copied());
            }
        }
        if indices.len() > maximum {
            return Err(HunterError::AffectedRouteLimit);
        }
        Ok(indices.into_iter().collect())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HunterMode {
    Shadow,
    DryRun,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HunterEconomicConfig {
    pub flash_premium_bps: u16,
    pub gas_cost: u128,
    pub tick_crossing_gas_cost: u128,
    pub ordering_cost_reserve: u128,
    pub model_error_reserve_bps: u16,
    pub shadow_maximum_input: u128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HunterEvent {
    pub origin_event_id: String,
    pub origin_router: String,
    pub chain_id: u64,
    pub block_number: u64,
    pub block_hash: String,
    pub observed_at_unix_ms: u64,
    pub evaluated_at_unix_ms: u64,
    pub touched_pool_addresses: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateBindings {
    pub risk_snapshot_hash: String,
    pub submission_quote_hash: String,
    pub executor_address: String,
    pub executor_code_hash: String,
    pub submission_channel: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct HunterMetrics {
    pub events_observed: u64,
    pub pools_matched: u64,
    pub routes_affected: u64,
    pub routes_evaluated: u64,
    pub candidates_produced: u64,
    pub candidates_rejected_by_economics: u64,
    pub state_incomplete: u64,
    pub size_probes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HunterProcessResult {
    pub candidates: Vec<Value>,
    pub affected_route_fingerprints: Vec<String>,
    pub metrics: HunterMetrics,
}

pub trait CandidateSink {
    fn materialize(&mut self, candidate: Value) -> Result<bool, HunterError>;
}

#[derive(Clone, Debug, Default)]
pub struct InMemoryCandidateSink {
    candidates: BTreeMap<String, Value>,
}

impl InMemoryCandidateSink {
    pub fn candidates(&self) -> impl Iterator<Item = &Value> {
        self.candidates.values()
    }

    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }
}

impl CandidateSink for InMemoryCandidateSink {
    fn materialize(&mut self, candidate: Value) -> Result<bool, HunterError> {
        let hash = candidate
            .get("candidate_hash")
            .and_then(Value::as_str)
            .filter(|hash| canonical_digest(hash))
            .ok_or(HunterError::CandidateIntegrity)?
            .to_string();
        if let Some(existing) = self.candidates.get(&hash) {
            if existing != &candidate {
                return Err(HunterError::CandidateIntegrity);
            }
            return Ok(false);
        }
        self.candidates.insert(hash, candidate);
        Ok(true)
    }
}

#[derive(Clone, Debug)]
pub struct HunterCore {
    mode: HunterMode,
    graph: HunterRouteGraph,
    bounds: HunterBounds,
    economics: HunterEconomicConfig,
    seen_order: VecDeque<String>,
    seen: HashSet<String>,
    metrics: HunterMetrics,
}

impl HunterCore {
    pub fn new(
        mode: HunterMode,
        graph: HunterRouteGraph,
        bounds: HunterBounds,
        economics: HunterEconomicConfig,
    ) -> Result<Self, HunterError> {
        let bounds = bounds.validate()?;
        if economics.flash_premium_bps > 10_000
            || economics.model_error_reserve_bps > 10_000
            || economics.shadow_maximum_input == 0
        {
            return Err(HunterError::InvalidBounds);
        }
        Ok(Self {
            mode,
            graph,
            bounds,
            economics,
            seen_order: VecDeque::new(),
            seen: HashSet::new(),
            metrics: HunterMetrics::default(),
        })
    }

    pub fn process_event<S: CandidateSink>(
        &mut self,
        event: &HunterEvent,
        states: &BTreeMap<String, ProviderStateAgreement>,
        bindings: &CandidateBindings,
        sink: &mut S,
    ) -> Result<HunterProcessResult, HunterError> {
        validate_event(event)?;
        validate_bindings(bindings)?;
        self.metrics.events_observed = self.metrics.events_observed.saturating_add(1);
        let affected_limit = self
            .bounds
            .maximum_affected_routes_per_event
            .min(self.graph.summary.shadow_enabled_route_count.max(1));
        let route_indices = self
            .graph
            .affected_route_indices(&event.touched_pool_addresses, affected_limit)?;
        let matched_pools = event
            .touched_pool_addresses
            .iter()
            .filter(|pool| self.graph.affected.contains_key(*pool))
            .count();
        self.metrics.pools_matched = self
            .metrics
            .pools_matched
            .saturating_add(matched_pools as u64);
        self.metrics.routes_affected = self
            .metrics
            .routes_affected
            .saturating_add(route_indices.len() as u64);

        let mut produced = Vec::new();
        let mut fingerprints = Vec::new();
        for index in route_indices {
            if produced.len() >= self.bounds.maximum_candidate_outputs_per_event {
                break;
            }
            let route = self
                .graph
                .bound_routes
                .get(index)
                .ok_or(HunterError::InvalidRoute)?
                .clone();
            fingerprints.push(route.route_fingerprint.clone());
            let dedupe_key = format!(
                "{}:{}:{}:{}",
                event.origin_event_id,
                event.block_number,
                event.block_hash,
                route.route_fingerprint
            );
            if !self.remember(dedupe_key) {
                continue;
            }
            self.metrics.routes_evaluated = self.metrics.routes_evaluated.saturating_add(1);
            match optimize_route(&route, states, &self.economics, self.bounds, event) {
                Ok(Some(optimized)) => {
                    self.metrics.size_probes = self
                        .metrics
                        .size_probes
                        .saturating_add(optimized.probes as u64);
                    let candidate = materialize_candidate(
                        self.mode,
                        &self.graph.universe_hash,
                        &route,
                        event,
                        bindings,
                        optimized,
                    )?;
                    if sink.materialize(candidate.clone())? {
                        self.metrics.candidates_produced =
                            self.metrics.candidates_produced.saturating_add(1);
                        produced.push(candidate);
                    }
                }
                Ok(None) => {
                    self.metrics.candidates_rejected_by_economics = self
                        .metrics
                        .candidates_rejected_by_economics
                        .saturating_add(1);
                }
                Err(HunterError::StateIncomplete) => {
                    self.metrics.state_incomplete = self.metrics.state_incomplete.saturating_add(1);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(HunterProcessResult {
            candidates: produced,
            affected_route_fingerprints: fingerprints,
            metrics: self.metrics.clone(),
        })
    }

    pub fn metrics(&self) -> &HunterMetrics {
        &self.metrics
    }

    fn remember(&mut self, key: String) -> bool {
        if !self.seen.insert(key.clone()) {
            return false;
        }
        self.seen_order.push_back(key);
        while self.seen.len() > MAX_SEEN_EVENT_ROUTES {
            if let Some(oldest) = self.seen_order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
        true
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum HunterError {
    #[error("hunter bounds are invalid")]
    InvalidBounds,
    #[error("route universe contract is invalid")]
    InvalidUniverse,
    #[error("route policy contract is invalid")]
    InvalidPolicy,
    #[error("route policy does not match an enumerable route")]
    PolicyRouteMismatch,
    #[error("hunter route is invalid")]
    InvalidRoute,
    #[error("duplicate semantic hunter route")]
    DuplicateRoute,
    #[error("hunter route enumeration exceeded a bound")]
    RouteLimit,
    #[error("affected route selection exceeded a bound")]
    AffectedRouteLimit,
    #[error("hunter event is invalid")]
    InvalidEvent,
    #[error("hunter pinned state is incomplete")]
    StateIncomplete,
    #[error("hunter pinned state failed integrity")]
    StateIntegrity,
    #[error("hunter economic policy rejected the simulation")]
    EconomicInfeasible,
    #[error("hunter arithmetic failed closed")]
    Arithmetic,
    #[error("hunter candidate binding is invalid")]
    CandidateIntegrity,
    #[error("hunter calldata plan is invalid")]
    PlanIntegrity,
}

impl HunterError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidBounds => "hunter_invalid_bounds",
            Self::InvalidUniverse => "hunter_invalid_universe",
            Self::InvalidPolicy => "hunter_invalid_policy",
            Self::PolicyRouteMismatch => "hunter_policy_route_mismatch",
            Self::InvalidRoute => "hunter_invalid_route",
            Self::DuplicateRoute => "hunter_duplicate_route",
            Self::RouteLimit => "hunter_route_limit",
            Self::AffectedRouteLimit => "hunter_affected_route_limit",
            Self::InvalidEvent => "hunter_invalid_event",
            Self::StateIncomplete => "hunter_state_incomplete",
            Self::StateIntegrity => "hunter_state_integrity",
            Self::EconomicInfeasible => "hunter_economic_infeasible",
            Self::Arithmetic => "hunter_arithmetic",
            Self::CandidateIntegrity => "hunter_candidate_integrity",
            Self::PlanIntegrity => "hunter_plan_integrity",
        }
    }
}

impl From<DomainError> for HunterError {
    fn from(error: DomainError) -> Self {
        match error {
            DomainError::StateIncomplete => Self::StateIncomplete,
            _ => Self::Arithmetic,
        }
    }
}

impl From<HunterStateError> for HunterError {
    fn from(error: HunterStateError) -> Self {
        match error {
            HunterStateError::ProviderDisagreement
            | HunterStateError::HashMismatch
            | HunterStateError::InvalidContract => Self::StateIntegrity,
            HunterStateError::LimitExceeded => Self::StateIncomplete,
        }
    }
}

fn validate_universe(universe: &RouteUniverse, bounds: HunterBounds) -> Result<(), HunterError> {
    let assets = universe
        .settlement_assets
        .iter()
        .chain(universe.intermediate_assets.iter())
        .collect::<Vec<_>>();
    if universe.schema_version != ROUTE_UNIVERSE_SCHEMA
        || universe.chain_id != 42_161
        || universe.universe_version == 0
        || universe.maximum_route_legs < 2
        || universe.maximum_route_legs > 4
        || universe.maximum_total_routes == 0
        || universe.maximum_total_routes > bounds.maximum_routes
        || universe.maximum_routes_per_event == 0
        || universe.maximum_routes_per_event > bounds.maximum_affected_routes_per_event
        || universe.default_hard_caps.maximum_tick_crossings == 0
        || universe.default_hard_caps.maximum_tick_crossings > bounds.maximum_tick_crossings_per_leg
        || universe.default_hard_caps.maximum_size_evaluations == 0
        || universe.default_hard_caps.maximum_size_evaluations > bounds.maximum_size_probes
        || assets.is_empty()
        || assets.len() > bounds.maximum_assets
        || universe.pools.is_empty()
        || universe.pools.len() > bounds.maximum_pools
    {
        return Err(HunterError::InvalidUniverse);
    }
    let mut asset_addresses = HashSet::new();
    for asset in assets {
        if !canonical_address(&asset.address)
            || !asset_addresses.insert(asset.address.clone())
            || asset.decimals == 0
            || asset.decimals > 36
            || parse_u128(&asset.maximum_input_amount)? == 0
        {
            return Err(HunterError::InvalidUniverse);
        }
    }
    let factory_addresses = universe
        .factories
        .iter()
        .map(|factory| factory.address.clone())
        .collect::<HashSet<_>>();
    if factory_addresses.len() != universe.factories.len()
        || universe
            .factories
            .iter()
            .any(|factory| !canonical_address(&factory.address))
    {
        return Err(HunterError::InvalidUniverse);
    }
    let mut pool_ids = HashSet::new();
    let mut pool_addresses = HashSet::new();
    for pool in &universe.pools {
        if !pool_ids.insert(pool.pool_id.clone())
            || !pool_addresses.insert(pool.address.clone())
            || !canonical_address(&pool.address)
            || !factory_addresses.contains(&pool.factory_address)
            || !asset_addresses.contains(&pool.token0)
            || !asset_addresses.contains(&pool.token1)
            || pool.token0 >= pool.token1
            || pool.fee == 0
            || pool.fee >= 1_000_000
            || pool.tick_spacing <= 0
        {
            return Err(HunterError::InvalidUniverse);
        }
    }
    if parse_u128(&universe.default_hard_caps.global_maximum_input_amount)? == 0 {
        return Err(HunterError::InvalidUniverse);
    }
    Ok(())
}

fn validate_policy(policy: &RoutePolicy, universe: &RouteUniverse) -> Result<(), HunterError> {
    let legs = policy.pool_addresses.len();
    if policy.schema_version != ROUTE_POLICY_SCHEMA
        || policy.chain_id != 42_161
        || policy.policy_version == 0
        || policy.route_universe_hash != universe.universe_hash
        || legs < 2
        || legs > universe.maximum_route_legs
        || policy.token_path.len() != legs + 1
        || [
            policy.factory_addresses.len(),
            policy.protocol_ids.len(),
            policy.fees.len(),
            policy.directions.len(),
        ]
        .iter()
        .any(|length| *length != legs)
        || policy.token_path.first() != Some(&policy.settlement_asset)
        || policy.token_path.last() != Some(&policy.settlement_asset)
        || parse_u128(&policy.minimum_input_amount)? == 0
        || parse_u128(&policy.maximum_input_amount)? < parse_u128(&policy.minimum_input_amount)?
        || parse_u128(&policy.minimum_retained_profit)? == 0
        || policy.maximum_tick_crossings == 0
        || policy.maximum_tick_crossings > universe.default_hard_caps.maximum_tick_crossings
        || policy.maximum_candidate_age_ms == 0
        || !policy.enabled_for_shadow
        || policy.enabled_for_autonomous_live
    {
        return Err(HunterError::InvalidPolicy);
    }
    Ok(())
}

fn enumerate_routes(
    universe: &RouteUniverse,
    bounds: HunterBounds,
) -> Result<Vec<EnumerableRoute>, HunterError> {
    let mut outgoing: BTreeMap<String, Vec<HunterRouteLeg>> = BTreeMap::new();
    for pool in &universe.pools {
        for (token_in, token_out, direction) in [
            (&pool.token0, &pool.token1, Direction::ZeroForOne),
            (&pool.token1, &pool.token0, Direction::OneForZero),
        ] {
            outgoing
                .entry(token_in.clone())
                .or_default()
                .push(HunterRouteLeg {
                    factory_address: pool.factory_address.clone(),
                    pool_id: pool.pool_id.clone(),
                    pool_address: pool.address.clone(),
                    protocol_id: pool.protocol_id.clone(),
                    token_in: token_in.clone(),
                    token_out: token_out.clone(),
                    fee: pool.fee,
                    tick_spacing: pool.tick_spacing,
                    direction,
                });
        }
    }
    for edges in outgoing.values_mut() {
        edges.sort_by_key(semantic_leg_key);
        edges.dedup_by(|left, right| semantic_leg_key(left) == semantic_leg_key(right));
    }

    let route_limit = universe.maximum_total_routes.min(bounds.maximum_routes);
    let mut routes = BTreeMap::new();
    let mut settlements = universe
        .settlement_assets
        .iter()
        .map(|asset| asset.address.clone())
        .collect::<Vec<_>>();
    settlements.sort();
    for settlement in settlements {
        let before = routes.len();
        let mut used_pools = HashSet::new();
        let mut visited_assets = HashSet::from([settlement.clone()]);
        let mut legs = Vec::new();
        enumerate_from(
            &settlement,
            &settlement,
            universe.maximum_route_legs,
            &outgoing,
            &mut used_pools,
            &mut visited_assets,
            &mut legs,
            &mut routes,
            route_limit,
        )?;
        if routes.len().saturating_sub(before) > bounds.maximum_cycles_per_settlement_asset {
            return Err(HunterError::RouteLimit);
        }
    }
    Ok(routes.into_values().collect())
}

#[allow(clippy::too_many_arguments)]
fn enumerate_from(
    settlement: &str,
    current: &str,
    maximum_legs: usize,
    outgoing: &BTreeMap<String, Vec<HunterRouteLeg>>,
    used_pools: &mut HashSet<String>,
    visited_assets: &mut HashSet<String>,
    legs: &mut Vec<HunterRouteLeg>,
    routes: &mut BTreeMap<String, EnumerableRoute>,
    route_limit: usize,
) -> Result<(), HunterError> {
    if legs.len() >= maximum_legs {
        return Ok(());
    }
    let Some(edges) = outgoing.get(current) else {
        return Ok(());
    };
    for edge in edges {
        if used_pools.contains(&edge.pool_address) {
            continue;
        }
        let closes = edge.token_out == settlement;
        if !closes && visited_assets.contains(&edge.token_out) {
            continue;
        }
        used_pools.insert(edge.pool_address.clone());
        if !closes {
            visited_assets.insert(edge.token_out.clone());
        }
        legs.push(edge.clone());
        if closes {
            if legs.len() >= 2 {
                let semantic_hash = route_semantic_hash(settlement, legs)?;
                routes
                    .entry(semantic_hash.clone())
                    .or_insert_with(|| EnumerableRoute {
                        semantic_hash,
                        settlement_asset: settlement.to_string(),
                        legs: legs.clone(),
                    });
                if routes.len() > route_limit {
                    return Err(HunterError::RouteLimit);
                }
            }
        } else {
            enumerate_from(
                settlement,
                &edge.token_out,
                maximum_legs,
                outgoing,
                used_pools,
                visited_assets,
                legs,
                routes,
                route_limit,
            )?;
        }
        legs.pop();
        if !closes {
            visited_assets.remove(&edge.token_out);
        }
        used_pools.remove(&edge.pool_address);
    }
    Ok(())
}

fn build_affected_index(
    routes: &[BoundRoute],
    bounds: HunterBounds,
) -> Result<BTreeMap<String, Vec<usize>>, HunterError> {
    let mut affected: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, route) in routes.iter().enumerate() {
        for leg in &route.route.legs {
            let indices = affected.entry(leg.pool_address.clone()).or_default();
            if !indices.contains(&index) {
                indices.push(index);
            }
            if indices.len() > bounds.maximum_routes_per_pool {
                return Err(HunterError::RouteLimit);
            }
        }
    }
    Ok(affected)
}

fn policy_matches_route(policy: &RoutePolicy, route: &EnumerableRoute) -> bool {
    policy.settlement_asset == route.settlement_asset
        && policy.token_path
            == std::iter::once(route.settlement_asset.clone())
                .chain(route.legs.iter().map(|leg| leg.token_out.clone()))
                .collect::<Vec<_>>()
        && policy.pool_addresses
            == route
                .legs
                .iter()
                .map(|leg| leg.pool_address.clone())
                .collect::<Vec<_>>()
        && policy.factory_addresses
            == route
                .legs
                .iter()
                .map(|leg| leg.factory_address.clone())
                .collect::<Vec<_>>()
        && policy.protocol_ids
            == route
                .legs
                .iter()
                .map(|leg| leg.protocol_id.clone())
                .collect::<Vec<_>>()
        && policy.fees == route.legs.iter().map(|leg| leg.fee).collect::<Vec<_>>()
        && policy.directions
            == route
                .legs
                .iter()
                .map(|leg| match leg.direction {
                    Direction::ZeroForOne => "zero_for_one".to_string(),
                    Direction::OneForZero => "one_for_zero".to_string(),
                })
                .collect::<Vec<_>>()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LegSimulation {
    pub pool_state_hash: String,
    pub amount_in: String,
    pub amount_out: String,
    pub ticks_crossed: u32,
    pub price_impact_bps: u16,
    pub gas_estimate_contribution: u64,
    pub minimum_output: String,
}

#[derive(Clone, Debug)]
struct RouteEvaluation {
    selected_size: u128,
    final_output: u128,
    gross_profit: i128,
    flash_premium: u128,
    gas_cost: u128,
    ordering_cost_reserve: u128,
    model_error_reserve: u128,
    total_cost: u128,
    conservative_net_pnl: i128,
    legs: Vec<LegSimulation>,
    state_hash: String,
    probes: usize,
}

fn optimize_route(
    route: &BoundRoute,
    states: &BTreeMap<String, ProviderStateAgreement>,
    economics: &HunterEconomicConfig,
    bounds: HunterBounds,
    event: &HunterEvent,
) -> Result<Option<RouteEvaluation>, HunterError> {
    let minimum = parse_u128(&route.policy.minimum_input_amount)?;
    let maximum = [
        parse_u128(&route.policy.maximum_input_amount)?,
        route.settlement_maximum_input,
        route.universe_maximum_input,
        economics.shadow_maximum_input,
    ]
    .into_iter()
    .min()
    .ok_or(HunterError::InvalidPolicy)?;
    if maximum < minimum {
        return Ok(None);
    }
    let maximum_probes = bounds.maximum_size_probes.min(32);
    let mut sizes = Vec::new();
    let mut size = minimum;
    while sizes.len() < maximum_probes.saturating_sub(bounds.maximum_local_refinements) {
        sizes.push(size);
        if size >= maximum {
            break;
        }
        size = size.saturating_mul(2).min(maximum);
        if sizes.last() == Some(&size) {
            break;
        }
    }
    if sizes.last() != Some(&maximum) && sizes.len() < maximum_probes {
        sizes.push(maximum);
    }
    let mut evaluated = BTreeMap::new();
    let mut saw_economic_infeasible = false;
    for size in sizes.iter().copied() {
        match evaluate_route(route, states, economics, bounds, event, size) {
            Ok(value) => {
                evaluated.insert(size, value);
            }
            Err(HunterError::StateIncomplete) => {}
            Err(HunterError::EconomicInfeasible) => saw_economic_infeasible = true,
            Err(error) => return Err(error),
        }
    }
    if evaluated.is_empty() {
        return if saw_economic_infeasible {
            Ok(None)
        } else {
            Err(HunterError::StateIncomplete)
        };
    }
    for _ in 0..bounds.maximum_local_refinements {
        if evaluated.len() >= maximum_probes {
            break;
        }
        let best_size = best_evaluation(&evaluated)
            .map(|value| value.selected_size)
            .ok_or(HunterError::Arithmetic)?;
        let lower = evaluated
            .range(..best_size)
            .next_back()
            .map(|(size, _)| *size);
        let upper = evaluated
            .range((best_size.saturating_add(1))..)
            .next()
            .map(|(size, _)| *size);
        let candidate = match (lower, upper) {
            (Some(lower), Some(upper)) => {
                let left_mid = lower + (best_size - lower) / 2;
                let right_mid = best_size + (upper - best_size) / 2;
                [left_mid, right_mid]
                    .into_iter()
                    .filter(|size| *size > lower && *size < upper)
                    .find(|size| !evaluated.contains_key(size))
            }
            (Some(lower), None) => {
                let midpoint = lower + (best_size - lower) / 2;
                (midpoint > lower && midpoint < best_size && !evaluated.contains_key(&midpoint))
                    .then_some(midpoint)
            }
            (None, Some(upper)) => {
                let midpoint = best_size + (upper - best_size) / 2;
                (midpoint > best_size && midpoint < upper && !evaluated.contains_key(&midpoint))
                    .then_some(midpoint)
            }
            (None, None) => None,
        };
        let Some(candidate) = candidate else {
            break;
        };
        match evaluate_route(route, states, economics, bounds, event, candidate) {
            Ok(value) => {
                evaluated.insert(candidate, value);
            }
            Err(HunterError::StateIncomplete | HunterError::EconomicInfeasible) => {}
            Err(error) => return Err(error),
        }
    }
    let probes = evaluated.len();
    let mut best = best_evaluation(&evaluated)
        .cloned()
        .ok_or(HunterError::Arithmetic)?;
    best.probes = probes;
    let retained = i128::try_from(parse_u128(&route.policy.minimum_retained_profit)?)
        .map_err(|_| HunterError::Arithmetic)?;
    if best.conservative_net_pnl <= retained || best.conservative_net_pnl <= 0 {
        return Ok(None);
    }
    Ok(Some(best))
}

fn best_evaluation(evaluated: &BTreeMap<u128, RouteEvaluation>) -> Option<&RouteEvaluation> {
    evaluated.values().max_by(|left, right| {
        left.conservative_net_pnl
            .cmp(&right.conservative_net_pnl)
            .then_with(|| right.selected_size.cmp(&left.selected_size))
    })
}

fn evaluate_route(
    route: &BoundRoute,
    states: &BTreeMap<String, ProviderStateAgreement>,
    economics: &HunterEconomicConfig,
    bounds: HunterBounds,
    event: &HunterEvent,
    input: u128,
) -> Result<RouteEvaluation, HunterError> {
    let event_age_ms = event
        .evaluated_at_unix_ms
        .checked_sub(event.observed_at_unix_ms)
        .ok_or(HunterError::InvalidEvent)?;
    if event_age_ms > route.policy.maximum_quote_age_ms
        || event_age_ms > route.policy.maximum_candidate_age_ms
    {
        return Err(HunterError::EconomicInfeasible);
    }
    let mut amount = input;
    let mut legs = Vec::with_capacity(route.route.legs.len());
    let mut state_hashes = Vec::with_capacity(route.route.legs.len());
    let mut total_tick_crossings = 0u128;
    for leg in &route.route.legs {
        let agreement = states
            .get(&leg.pool_address)
            .ok_or(HunterError::StateIncomplete)?;
        let state = agreement.agreed()?;
        validate_leg_state(leg, state, event, bounds)?;
        let simulation = simulate_pool_exact_input(
            state,
            amount,
            leg.direction,
            route
                .policy
                .maximum_tick_crossings
                .min(bounds.maximum_tick_crossings_per_leg),
        )?;
        let price_impact_bps =
            exact_price_impact_bps(state, amount, leg.direction, simulation.amount_out)?;
        if price_impact_bps > route.policy.maximum_price_impact_bps {
            return Err(HunterError::EconomicInfeasible);
        }
        let gas_estimate_contribution = 70_000u64
            .checked_add(u64::from(simulation.ticks_crossed).saturating_mul(15_000))
            .ok_or(HunterError::Arithmetic)?;
        total_tick_crossings = total_tick_crossings
            .checked_add(u128::from(simulation.ticks_crossed))
            .ok_or(HunterError::Arithmetic)?;
        let minimum_output =
            slippage_floor(simulation.amount_out, route.policy.maximum_slippage_bps)?;
        legs.push(LegSimulation {
            pool_state_hash: state.state_hash.clone(),
            amount_in: amount.to_string(),
            amount_out: simulation.amount_out.to_string(),
            ticks_crossed: simulation.ticks_crossed,
            price_impact_bps,
            gas_estimate_contribution,
            minimum_output: minimum_output.to_string(),
        });
        state_hashes.push(state.state_hash.clone());
        amount = simulation.amount_out;
    }
    let gross_profit = signed_difference(amount, input)?;
    let positive_gross =
        u128::try_from(gross_profit.max(0)).map_err(|_| HunterError::Arithmetic)?;
    let flash_premium = mul_div_ceil(
        input,
        u128::from(economics.flash_premium_bps),
        BPS_DENOMINATOR,
    )?;
    let model_error_reserve = mul_div_ceil(
        positive_gross,
        u128::from(economics.model_error_reserve_bps),
        BPS_DENOMINATOR,
    )?;
    let gas_cost = economics
        .tick_crossing_gas_cost
        .checked_mul(total_tick_crossings)
        .and_then(|crossing_cost| economics.gas_cost.checked_add(crossing_cost))
        .ok_or(HunterError::Arithmetic)?;
    let total_cost = flash_premium
        .checked_add(gas_cost)
        .and_then(|value| value.checked_add(economics.ordering_cost_reserve))
        .and_then(|value| value.checked_add(model_error_reserve))
        .ok_or(HunterError::Arithmetic)?;
    let conservative_net_pnl = gross_profit
        .checked_sub(i128::try_from(total_cost).map_err(|_| HunterError::Arithmetic)?)
        .ok_or(HunterError::Arithmetic)?;
    let state_hash = digest_json(
        "phoenix.hunter-route-state.v1",
        &json!({
            "block_number": event.block_number,
            "block_hash": event.block_hash,
            "route_fingerprint": route.route_fingerprint,
            "pool_state_hashes": state_hashes
        }),
    )?;
    Ok(RouteEvaluation {
        selected_size: input,
        final_output: amount,
        gross_profit,
        flash_premium,
        gas_cost,
        ordering_cost_reserve: economics.ordering_cost_reserve,
        model_error_reserve,
        total_cost,
        conservative_net_pnl,
        legs,
        state_hash,
        probes: 0,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ExactPoolSwap {
    amount_out: u128,
    ticks_crossed: u32,
}

fn exact_price_impact_bps(
    state: &PinnedV3PoolState,
    amount_in: u128,
    direction: Direction,
    amount_out: u128,
) -> Result<u16, HunterError> {
    let amount_in_less_fee = amount_less_fee(Amount(amount_in), state.fee)?.0;
    let sqrt_price =
        U256::from_dec_str(&state.sqrt_price_x96).map_err(|_| HunterError::StateIntegrity)?;
    let spot_amount_out = spot_output(sqrt_price, amount_in_less_fee, direction)?;
    if spot_amount_out == 0 || amount_out > spot_amount_out {
        return Err(HunterError::Arithmetic);
    }
    let impact = mul_div_ceil(
        spot_amount_out - amount_out,
        BPS_DENOMINATOR,
        spot_amount_out,
    )?;
    u16::try_from(impact.min(BPS_DENOMINATOR)).map_err(|_| HunterError::Arithmetic)
}

fn simulate_pool_exact_input(
    state: &PinnedV3PoolState,
    amount_in: u128,
    direction: Direction,
    maximum_tick_crossings: u32,
) -> Result<ExactPoolSwap, HunterError> {
    if amount_in == 0 || maximum_tick_crossings == 0 {
        return Err(HunterError::Arithmetic);
    }
    let mut remaining = amount_in;
    let mut output = 0u128;
    let mut sqrt_price =
        U256::from_dec_str(&state.sqrt_price_x96).map_err(|_| HunterError::StateIntegrity)?;
    let mut liquidity = parse_u128(&state.liquidity)?;
    let mut tick = state.tick;
    let mut crossings = 0u32;
    while remaining > 0 {
        let next_initialized = match direction {
            Direction::ZeroForOne => state
                .initialized_ticks
                .iter()
                .rev()
                .find(|candidate| candidate.tick < tick),
            Direction::OneForZero => state
                .initialized_ticks
                .iter()
                .find(|candidate| candidate.tick > tick),
        };
        let (target_tick, initialized) = match next_initialized {
            Some(value) => (value.tick, true),
            None => match direction {
                Direction::ZeroForOne => (state.coverage_min_tick, false),
                Direction::OneForZero => (state.coverage_max_tick, false),
            },
        };
        if target_tick == tick {
            return Err(HunterError::StateIncomplete);
        }
        let target_sqrt = sqrt_ratio_at_tick(target_tick)?;
        let net_to_target = match direction {
            Direction::ZeroForOne => {
                u256_to_u128(amount_0_delta(target_sqrt, sqrt_price, liquidity, true)?)?
            }
            Direction::OneForZero => {
                u256_to_u128(amount_1_delta(sqrt_price, target_sqrt, liquidity, true)?)?
            }
        };
        if net_to_target == 0 {
            return Err(HunterError::StateIncomplete);
        }
        let gross_to_target = gross_from_net(net_to_target, state.fee)?;
        if remaining < gross_to_target {
            let net = amount_less_fee(Amount(remaining), state.fee)?.0;
            if net == 0 {
                return Err(HunterError::Arithmetic);
            }
            let next = next_sqrt_price_from_input(sqrt_price, liquidity, net, direction)?;
            let step_output = match direction {
                Direction::ZeroForOne => {
                    u256_to_u128(amount_1_delta(next, sqrt_price, liquidity, false)?)?
                }
                Direction::OneForZero => {
                    u256_to_u128(amount_0_delta(sqrt_price, next, liquidity, false)?)?
                }
            };
            output = output
                .checked_add(step_output)
                .ok_or(HunterError::Arithmetic)?;
            remaining = 0;
            continue;
        }
        if !initialized {
            return Err(HunterError::StateIncomplete);
        }
        let step_output = match direction {
            Direction::ZeroForOne => {
                u256_to_u128(amount_1_delta(target_sqrt, sqrt_price, liquidity, false)?)?
            }
            Direction::OneForZero => {
                u256_to_u128(amount_0_delta(sqrt_price, target_sqrt, liquidity, false)?)?
            }
        };
        output = output
            .checked_add(step_output)
            .ok_or(HunterError::Arithmetic)?;
        remaining = remaining
            .checked_sub(gross_to_target)
            .ok_or(HunterError::Arithmetic)?;
        sqrt_price = target_sqrt;
        crossings = crossings.saturating_add(1);
        if crossings > maximum_tick_crossings {
            return Err(HunterError::StateIncomplete);
        }
        let tick_evidence = next_initialized.ok_or(HunterError::StateIncomplete)?;
        let liquidity_net = tick_evidence
            .liquidity_net
            .parse::<i128>()
            .map_err(|_| HunterError::StateIntegrity)?;
        liquidity = apply_liquidity_net(liquidity, liquidity_net, direction)?;
        tick = match direction {
            Direction::ZeroForOne => target_tick.checked_sub(1).ok_or(HunterError::Arithmetic)?,
            Direction::OneForZero => target_tick,
        };
    }
    if output == 0 {
        return Err(HunterError::Arithmetic);
    }
    Ok(ExactPoolSwap {
        amount_out: output,
        ticks_crossed: crossings,
    })
}

fn apply_liquidity_net(
    liquidity: u128,
    liquidity_net: i128,
    direction: Direction,
) -> Result<u128, HunterError> {
    let signed = match direction {
        Direction::ZeroForOne => liquidity_net.checked_neg().ok_or(HunterError::Arithmetic)?,
        Direction::OneForZero => liquidity_net,
    };
    let updated = if signed >= 0 {
        liquidity.checked_add(signed as u128)
    } else {
        liquidity.checked_sub(signed.unsigned_abs())
    }
    .ok_or(HunterError::Arithmetic)?;
    if updated == 0 {
        return Err(HunterError::StateIncomplete);
    }
    Ok(updated)
}

fn materialize_candidate(
    mode: HunterMode,
    universe_hash: &str,
    route: &BoundRoute,
    event: &HunterEvent,
    bindings: &CandidateBindings,
    evaluation: RouteEvaluation,
) -> Result<Value, HunterError> {
    if !matches!(mode, HunterMode::Shadow | HunterMode::DryRun) {
        return Err(HunterError::CandidateIntegrity);
    }
    let expires_ms = event
        .observed_at_unix_ms
        .checked_add(route.policy.maximum_candidate_age_ms)
        .ok_or(HunterError::Arithmetic)?;
    let created = canonical_timestamp(event.evaluated_at_unix_ms)?;
    let expires = canonical_timestamp(expires_ms)?;
    let minimum_leg_outputs = evaluation
        .legs
        .iter()
        .map(|leg| parse_u128(&leg.minimum_output))
        .collect::<Result<Vec<_>, _>>()?;
    let calldata = encode_shadow_calldata(
        &route.route,
        &event.origin_router,
        &bindings.executor_address,
        evaluation.selected_size,
        route
            .universe_maximum_input
            .min(parse_u128(&route.policy.maximum_input_amount)?),
        parse_u128(&route.policy.minimum_retained_profit)?
            .checked_add(evaluation.gas_cost)
            .ok_or(HunterError::Arithmetic)?,
        expires_ms.div_ceil(1_000),
        &minimum_leg_outputs,
    )?;
    let calldata_hash = hex::encode(Sha256::digest(calldata));
    let plan = json!({
        "schema_version": "phoenix.hunter-shadow-plan.v1",
        "mode": match mode { HunterMode::Shadow => "shadow", HunterMode::DryRun => "dry_run" },
        "route_fingerprint": route.route_fingerprint,
        "route_semantic_hash": route.route.semantic_hash,
        "route_policy_hash": route.policy.policy_hash,
        "route_universe_hash": universe_hash,
        "block_number": event.block_number,
        "block_hash": event.block_hash,
        "state_hash": evaluation.state_hash,
        "selected_input": evaluation.selected_size.to_string(),
        "final_output": evaluation.final_output.to_string(),
        "legs": evaluation.legs,
        "economics": {
            "predicted_gross_profit": evaluation.gross_profit.to_string(),
            "flash_premium": evaluation.flash_premium.to_string(),
            "gas_cost": evaluation.gas_cost.to_string(),
            "ordering_cost_reserve": evaluation.ordering_cost_reserve.to_string(),
            "model_error_reserve": evaluation.model_error_reserve.to_string(),
            "predicted_total_cost": evaluation.total_cost.to_string(),
            "conservative_predicted_net_pnl": evaluation.conservative_net_pnl.to_string()
        },
        "calldata_hash": calldata_hash,
        "executor_address": bindings.executor_address,
        "executor_code_hash": bindings.executor_code_hash,
        "unsigned": true,
        "shadow_only": true,
        "execution_eligible": false,
        "execution_request_created": false,
        "signer_used": false,
        "public_broadcast": false
    });
    let plan_hash = digest_json("phoenix.hunter-shadow-plan.v1", &plan)?;
    let identity_seed = format!(
        "{}:{}:{}:{}",
        event.origin_event_id, route.route_fingerprint, event.block_hash, plan_hash
    );
    let candidate_id = deterministic_uuid("candidate", &identity_seed);
    let opportunity_id = deterministic_uuid("opportunity", &identity_seed);
    let predicted_gross_profit = evaluation.gross_profit.max(0);
    let mut candidate = json!({
        "schema_version": CANDIDATE_SCHEMA,
        "candidate_id": candidate_id,
        "opportunity_id": opportunity_id,
        "origin_event_id": event.origin_event_id,
        "chain_id": 42161,
        "route_fingerprint": route.route_fingerprint,
        "route_universe_hash": universe_hash,
        "route_policy_hash": route.policy.policy_hash,
        "risk_policy_hash": route.policy.policy_hash,
        "state_block_number": event.block_number,
        "state_block_hash": event.block_hash,
        "state_hash": evaluation.state_hash,
        "selected_size": evaluation.selected_size.to_string(),
        "predicted_gross_profit": predicted_gross_profit.to_string(),
        "predicted_total_cost": evaluation.total_cost.to_string(),
        "conservative_predicted_net_pnl": evaluation.conservative_net_pnl.to_string(),
        "plan_hash": plan_hash,
        "calldata_hash": calldata_hash,
        "executor_address": bindings.executor_address,
        "executor_code_hash": bindings.executor_code_hash,
        "submission_channel": bindings.submission_channel,
        "submission_quote_hash": bindings.submission_quote_hash,
        "risk_snapshot_hash": bindings.risk_snapshot_hash,
        "candidate_created_at": created,
        "candidate_expires_at": expires,
        "status": "materialized",
        "candidate_hash": "0".repeat(64)
    });
    let hash = contract_hash(
        &candidate,
        "candidate_hash",
        "autonomous-candidate",
        CANDIDATE_SCHEMA,
    )?;
    candidate["candidate_hash"] = Value::String(hash);
    Ok(candidate)
}

#[allow(clippy::too_many_arguments)]
fn encode_shadow_calldata(
    route: &EnumerableRoute,
    origin_router: &str,
    executor_address: &str,
    input_amount: u128,
    maximum_input_amount: u128,
    minimum_profit: u128,
    deadline: u64,
    minimum_leg_outputs: &[u128],
) -> Result<Vec<u8>, HunterError> {
    if minimum_leg_outputs.len() != route.legs.len() {
        return Err(HunterError::PlanIntegrity);
    }
    let legs = route
        .legs
        .iter()
        .enumerate()
        .map(|(index, leg)| {
            Ok(Token::Tuple(vec![
                Token::Address(parse_eth_address(&leg.pool_address)?),
                Token::Address(parse_eth_address(&leg.token_in)?),
                Token::Address(parse_eth_address(&leg.token_out)?),
                Token::Uint(U256::from(leg.fee)),
                Token::Bool(matches!(leg.direction, Direction::ZeroForOne)),
                Token::Uint(U256::from(minimum_leg_outputs[index])),
            ]))
        })
        .collect::<Result<Vec<_>, HunterError>>()?;
    let route_hash = hex::decode(&route.semantic_hash).map_err(|_| HunterError::PlanIntegrity)?;
    let opportunity = Token::Tuple(vec![
        Token::FixedBytes(route_hash),
        Token::Address(parse_eth_address(origin_router)?),
        Token::Address(parse_eth_address(executor_address)?),
        Token::Address(parse_eth_address(&route.settlement_asset)?),
        Token::Uint(U256::from(input_amount)),
        Token::Uint(U256::from(maximum_input_amount)),
        Token::Uint(U256::from(minimum_profit)),
        Token::Uint(U256::from(deadline)),
        Token::Array(legs),
    ]);
    let contract = Contract::load(Cursor::new(include_bytes!(
        "../../../fork-sandbox/abi/PhoenixExecutor.json"
    )))
    .map_err(|_| HunterError::PlanIntegrity)?;
    contract
        .function("executeOpportunity")
        .and_then(|function| function.encode_input(&[opportunity]))
        .map_err(|_| HunterError::PlanIntegrity)
}

fn validate_leg_state(
    leg: &HunterRouteLeg,
    state: &PinnedV3PoolState,
    event: &HunterEvent,
    bounds: HunterBounds,
) -> Result<(), HunterError> {
    if state.schema_version != PINNED_V3_STATE_SCHEMA
        || state.chain_id != event.chain_id
        || state.block_number != event.block_number
        || state.block_hash != event.block_hash
        || state.pool_id != leg.pool_id
        || state.pool_address != leg.pool_address
        || state.factory_address != leg.factory_address
        || state.protocol_id != leg.protocol_id
        || state.fee != leg.fee
        || state.tick_spacing != leg.tick_spacing
        || state.tick_bitmap_words.len() > bounds.maximum_tick_words_per_pool
        || state.initialized_ticks.len() > bounds.maximum_initialized_ticks
    {
        return Err(HunterError::StateIntegrity);
    }
    let direction_matches = match leg.direction {
        Direction::ZeroForOne => state.token0 == leg.token_in && state.token1 == leg.token_out,
        Direction::OneForZero => state.token1 == leg.token_in && state.token0 == leg.token_out,
    };
    if !direction_matches {
        return Err(HunterError::StateIntegrity);
    }
    Ok(())
}

fn validate_event(event: &HunterEvent) -> Result<(), HunterError> {
    if event.chain_id != 42_161
        || event.block_number == 0
        || !canonical_prefixed_digest(&event.block_hash)
        || !canonical_address(&event.origin_router)
        || event.observed_at_unix_ms == 0
        || event.evaluated_at_unix_ms < event.observed_at_unix_ms
        || event.origin_event_id.is_empty()
        || event.origin_event_id.len() > 256
        || !event
            .origin_event_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
        || event.touched_pool_addresses.is_empty()
    {
        return Err(HunterError::InvalidEvent);
    }
    Ok(())
}

fn validate_bindings(bindings: &CandidateBindings) -> Result<(), HunterError> {
    if !canonical_digest(&bindings.risk_snapshot_hash)
        || !canonical_digest(&bindings.submission_quote_hash)
        || !canonical_address(&bindings.executor_address)
        || !canonical_digest(&bindings.executor_code_hash)
        || !matches!(
            bindings.submission_channel.as_str(),
            "standard_rpc" | "disabled_ordering"
        )
    {
        return Err(HunterError::CandidateIntegrity);
    }
    Ok(())
}

fn verify_contract_hash(
    value: &Value,
    hash_field: &str,
    domain: &str,
    schema: &str,
) -> Result<(), HunterError> {
    let actual = value
        .get(hash_field)
        .and_then(Value::as_str)
        .ok_or(HunterError::CandidateIntegrity)?;
    let expected = contract_hash(value, hash_field, domain, schema)?;
    if actual != expected {
        return Err(HunterError::CandidateIntegrity);
    }
    Ok(())
}

fn contract_hash(
    value: &Value,
    hash_field: &str,
    domain: &str,
    schema: &str,
) -> Result<String, HunterError> {
    let mut body = value.clone();
    body.as_object_mut()
        .ok_or(HunterError::CandidateIntegrity)?
        .remove(hash_field);
    digest_json(
        &format!("phoenix.canonical-json.v1:{domain}:{schema}"),
        &body,
    )
}

fn digest_json(domain: &str, value: &Value) -> Result<String, HunterError> {
    let mut bytes = domain.as_bytes().to_vec();
    bytes.push(b'\n');
    bytes.extend(canonical_json(value)?);
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn canonical_json(value: &Value) -> Result<Vec<u8>, HunterError> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) | Value::Number(_) => {
            serde_json::to_vec(value).map_err(|_| HunterError::CandidateIntegrity)
        }
        Value::Array(values) => {
            let mut output = vec![b'['];
            for (index, child) in values.iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                output.extend(canonical_json(child)?);
            }
            output.push(b']');
            Ok(output)
        }
        Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort();
            let mut output = vec![b'{'];
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                output
                    .extend(serde_json::to_vec(key).map_err(|_| HunterError::CandidateIntegrity)?);
                output.push(b':');
                output.extend(canonical_json(
                    values.get(key).ok_or(HunterError::CandidateIntegrity)?,
                )?);
            }
            output.push(b'}');
            Ok(output)
        }
    }
}

fn route_semantic_hash(settlement: &str, legs: &[HunterRouteLeg]) -> Result<String, HunterError> {
    digest_json(
        "phoenix.hunter-route.v1",
        &json!({"settlement_asset": settlement, "legs": legs}),
    )
}

fn semantic_leg_key(leg: &HunterRouteLeg) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}:{:?}",
        leg.factory_address,
        leg.pool_address,
        leg.token_in,
        leg.token_out,
        leg.fee,
        leg.protocol_id,
        leg.direction
    )
}

fn gross_from_net(net: u128, fee: u32) -> Result<u128, HunterError> {
    let retained = FEE_DENOMINATOR
        .checked_sub(u128::from(fee))
        .ok_or(HunterError::Arithmetic)?;
    u512_to_u128(div_rounding_up(
        U512::from(net)
            .checked_mul(U512::from(FEE_DENOMINATOR))
            .ok_or(HunterError::Arithmetic)?,
        U512::from(retained),
    )?)
    .map_err(Into::into)
}

fn slippage_floor(value: u128, bps: u16) -> Result<u128, HunterError> {
    let retained = BPS_DENOMINATOR
        .checked_sub(u128::from(bps))
        .ok_or(HunterError::Arithmetic)?;
    let output = value
        .checked_mul(retained)
        .map(|scaled| scaled / BPS_DENOMINATOR)
        .ok_or(HunterError::Arithmetic)?;
    if output == 0 {
        return Err(HunterError::Arithmetic);
    }
    Ok(output)
}

fn mul_div_ceil(value: u128, multiplier: u128, denominator: u128) -> Result<u128, HunterError> {
    if denominator == 0 {
        return Err(HunterError::Arithmetic);
    }
    let product = U512::from(value)
        .checked_mul(U512::from(multiplier))
        .ok_or(HunterError::Arithmetic)?;
    u512_to_u128(div_rounding_up(product, U512::from(denominator))?).map_err(Into::into)
}

fn signed_difference(lhs: u128, rhs: u128) -> Result<i128, HunterError> {
    if lhs >= rhs {
        i128::try_from(lhs - rhs).map_err(|_| HunterError::Arithmetic)
    } else {
        i128::try_from(rhs - lhs)
            .map_err(|_| HunterError::Arithmetic)?
            .checked_neg()
            .ok_or(HunterError::Arithmetic)
    }
}

fn parse_u128(value: &str) -> Result<u128, HunterError> {
    if value.is_empty()
        || value.len() > 39
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err(HunterError::Arithmetic);
    }
    value.parse().map_err(|_| HunterError::Arithmetic)
}

fn canonical_timestamp(unix_ms: u64) -> Result<String, HunterError> {
    let timestamp = i64::try_from(unix_ms).map_err(|_| HunterError::Arithmetic)?;
    Utc.timestamp_millis_opt(timestamp)
        .single()
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Secs, true))
        .ok_or(HunterError::Arithmetic)
}

fn deterministic_uuid(domain: &str, seed: &str) -> String {
    let mut bytes = Sha256::digest(format!("{domain}:{seed}")).to_vec();
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{}-{}-{}-{}-{}",
        hex::encode(&bytes[0..4]),
        hex::encode(&bytes[4..6]),
        hex::encode(&bytes[6..8]),
        hex::encode(&bytes[8..10]),
        hex::encode(&bytes[10..16])
    )
}

fn parse_eth_address(value: &str) -> Result<EthAddress, HunterError> {
    if !canonical_address(value) {
        return Err(HunterError::PlanIntegrity);
    }
    let bytes = hex::decode(&value[2..]).map_err(|_| HunterError::PlanIntegrity)?;
    Ok(EthAddress::from_slice(&bytes))
}

fn canonical_address(value: &str) -> bool {
    value.len() == 42
        && value.starts_with("0x")
        && value[2..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn canonical_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn canonical_prefixed_digest(value: &str) -> bool {
    value.len() == 66 && value.starts_with("0x") && canonical_digest(&value[2..])
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNIVERSE: &str = include_str!("../../../config/phoenix-route-universe-v1.json");
    const POLICY: &str =
        include_str!("../../../fixtures/autonomous-hunter/v1/valid/route-policy.json");
    const PINNED_FORK_CROSS_TICK: &str =
        include_str!("../../../fixtures/hunter-a1/v1/pinned-fork-cross-tick.json");
    const WETH: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
    const USDC: &str = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";
    const POOL_500: &str = "0xc6962004f452be9203591991d15f6b388e09e8d0";
    const POOL_3000: &str = "0xc473e2aee3441bf9240be85eb122abb059a3b57c";
    const FACTORY: &str = "0x1f98431c8ad98523631ae4a59f267346ea31f984";

    fn graph() -> HunterRouteGraph {
        HunterRouteGraph::from_contracts(UNIVERSE, &[POLICY], HunterBounds::default()).unwrap()
    }

    fn state(
        pool_id: &str,
        pool_address: &str,
        fee: u32,
        spacing: i32,
        tick: i32,
    ) -> PinnedV3PoolState {
        let mut value = PinnedV3PoolState {
            schema_version: PINNED_V3_STATE_SCHEMA.to_string(),
            chain_id: 42_161,
            block_number: 48_379_269,
            block_hash: format!("0x{}", "a".repeat(64)),
            pool_id: pool_id.to_string(),
            pool_address: pool_address.to_string(),
            factory_address: FACTORY.to_string(),
            protocol_id: "uniswap-v3".to_string(),
            token0: WETH.to_string(),
            token1: USDC.to_string(),
            fee,
            tick_spacing: spacing,
            sqrt_price_x96: sqrt_ratio_at_tick(tick).unwrap().to_string(),
            tick,
            liquidity: "1000000000000000000000000000000".to_string(),
            coverage_min_tick: tick - spacing * 4,
            coverage_max_tick: tick + spacing * 4,
            tick_bitmap_words: Vec::new(),
            initialized_ticks: Vec::new(),
            state_hash: "0".repeat(64),
        };
        value.state_hash = value.canonical_hash().unwrap();
        value
    }

    fn agreement(state: PinnedV3PoolState) -> ProviderStateAgreement {
        ProviderStateAgreement {
            primary_provider_id: "provider-primary".to_string(),
            secondary_provider_id: "provider-secondary".to_string(),
            primary: state.clone(),
            secondary: state,
        }
    }

    fn graph_universe() -> RouteUniverse {
        let addresses = [
            "0x1000000000000000000000000000000000000000",
            "0x2000000000000000000000000000000000000000",
            "0x3000000000000000000000000000000000000000",
            "0x4000000000000000000000000000000000000000",
        ];
        let asset = |index: usize| UniverseAsset {
            asset_id: format!("asset-{index}"),
            address: addresses[index].to_string(),
            symbol: format!("T{index}"),
            decimals: 18,
            maximum_input_amount: "1000000".to_string(),
        };
        let pool = |id: usize, left: usize, right: usize| UniversePool {
            pool_id: format!("pool-{id}"),
            protocol_id: "fixture-v3".to_string(),
            factory_address: "0x0100000000000000000000000000000000000000".to_string(),
            address: format!("0x{id:040x}"),
            token0: addresses[left.min(right)].to_string(),
            token1: addresses[left.max(right)].to_string(),
            fee: if id % 2 == 0 { 3_000 } else { 500 },
            tick_spacing: if id % 2 == 0 { 60 } else { 10 },
        };
        RouteUniverse {
            schema_version: ROUTE_UNIVERSE_SCHEMA.to_string(),
            universe_id: "fixture-graph".to_string(),
            universe_version: 1,
            chain_id: 42_161,
            settlement_assets: vec![asset(0)],
            intermediate_assets: vec![asset(1), asset(2), asset(3)],
            routers: Vec::new(),
            factories: vec![UniverseFactory {
                factory_id: "fixture-factory".to_string(),
                protocol_id: "fixture-v3".to_string(),
                address: "0x0100000000000000000000000000000000000000".to_string(),
                pool_init_code_hash: format!("0x{}", "1".repeat(64)),
            }],
            pools: vec![
                pool(1, 0, 1),
                pool(2, 0, 1),
                pool(3, 1, 2),
                pool(4, 0, 2),
                pool(5, 2, 3),
                pool(6, 0, 3),
            ],
            maximum_route_legs: 4,
            maximum_total_routes: 64,
            maximum_routes_per_event: 16,
            default_hard_caps: UniverseHardCaps {
                global_maximum_input_amount: "1000000".to_string(),
                maximum_tick_crossings: 64,
                maximum_size_evaluations: 16,
            },
            universe_hash: "0".repeat(64),
        }
    }

    #[test]
    fn reviewed_universe_retains_one_policy_route_and_enumerates_both_fee_directions() {
        let graph = graph();
        assert_eq!(graph.summary.asset_count, 2);
        assert_eq!(graph.summary.pool_count, 2);
        assert_eq!(graph.summary.enumerable_route_count, 2);
        assert_eq!(graph.summary.shadow_enabled_route_count, 1);
        assert_eq!(graph.summary.routes_per_leg_count.get(&2), Some(&2));
    }

    #[test]
    fn multigraph_enumerates_two_three_and_four_leg_cycles_independent_of_pool_order() {
        let universe = graph_universe();
        let routes = enumerate_routes(&universe, HunterBounds::default()).unwrap();
        let lengths = routes
            .iter()
            .map(|route| route.legs.len())
            .collect::<BTreeSet<_>>();
        assert_eq!(lengths, BTreeSet::from([2, 3, 4]));
        assert!(routes.iter().all(|route| {
            route
                .legs
                .iter()
                .map(|leg| &leg.pool_address)
                .collect::<HashSet<_>>()
                .len()
                == route.legs.len()
        }));
        let mut reordered = universe;
        reordered.pools.reverse();
        let reordered_routes = enumerate_routes(&reordered, HunterBounds::default()).unwrap();
        assert_eq!(routes, reordered_routes);
    }

    #[test]
    fn affected_index_is_pool_scoped_deduplicated_and_deterministic() {
        let graph = graph();
        let routes = graph
            .affected_route_indices(
                &[
                    POOL_3000.to_string(),
                    POOL_500.to_string(),
                    POOL_500.to_string(),
                ],
                16,
            )
            .unwrap();
        assert_eq!(routes, vec![0]);
        assert!(graph
            .affected_route_indices(
                &["0x9999999999999999999999999999999999999999".to_string()],
                16
            )
            .unwrap()
            .is_empty());
    }

    #[test]
    fn exact_swap_crosses_initialized_ticks_in_both_directions() {
        let fixture: Value = serde_json::from_str(PINNED_FORK_CROSS_TICK).unwrap();
        assert_eq!(
            fixture["schema_version"],
            "phoenix.hunter-pinned-fork-parity.v1"
        );
        let vectors = fixture["vectors"].as_array().unwrap();
        assert_eq!(vectors.len(), 2);
        for vector in vectors {
            let state: PinnedV3PoolState = serde_json::from_value(vector["state"].clone()).unwrap();
            state.validate().unwrap();
            let direction = match vector["direction"].as_str().unwrap() {
                "zero_for_one" => Direction::ZeroForOne,
                "one_for_zero" => Direction::OneForZero,
                value => panic!("unexpected fixture direction: {value}"),
            };
            let amount_in = vector["amount_in"].as_str().unwrap().parse().unwrap();
            let maximum_tick_crossings =
                u32::try_from(vector["maximum_tick_crossings"].as_u64().unwrap()).unwrap();
            let expected_amount_out = vector["expected_amount_out"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap();
            let expected_ticks_crossed =
                u32::try_from(vector["expected_ticks_crossed"].as_u64().unwrap()).unwrap();
            let result =
                simulate_pool_exact_input(&state, amount_in, direction, maximum_tick_crossings)
                    .unwrap();
            assert_eq!(result.amount_out, expected_amount_out);
            assert_eq!(result.ticks_crossed, expected_ticks_crossed);
            assert_eq!(
                exact_price_impact_bps(&state, amount_in, direction, result.amount_out).unwrap(),
                u16::try_from(vector["expected_price_impact_bps"].as_u64().unwrap()).unwrap()
            );
        }
    }

    #[test]
    fn unproven_tick_region_fails_closed_with_bounded_code() {
        let mut incomplete = state("pool", POOL_500, 500, 10, 0);
        incomplete.liquidity = "1000000".to_string();
        incomplete.coverage_min_tick = -10;
        incomplete.coverage_max_tick = 10;
        incomplete.state_hash = incomplete.canonical_hash().unwrap();
        let error = simulate_pool_exact_input(&incomplete, 1_000_000, Direction::ZeroForOne, 2)
            .unwrap_err();
        assert_eq!(error, HunterError::StateIncomplete);
        assert_eq!(error.code(), "hunter_state_incomplete");
        assert!(!error.code().contains("0x"));
    }

    #[test]
    fn current_range_fixture_remains_semantically_exact_in_both_directions() {
        use crate::amm::v3::simulate_current_range_exact_input;
        use crate::domain::{Address, Liquidity, PoolId, SqrtPriceX96, Tick, TokenAddress};
        use crate::state::{PoolState, StateCompleteness};

        let legacy = PoolState {
            pool_id: PoolId("pool".to_string()),
            token0: TokenAddress(Address::parse(WETH).unwrap()),
            token1: TokenAddress(Address::parse(USDC).unwrap()),
            fee: 500,
            tick: Tick(5),
            liquidity: Liquidity(1_000_000_000_000),
            sqrt_price_x96: SqrtPriceX96(sqrt_ratio_at_tick(5).unwrap()),
            completeness: StateCompleteness {
                min_tick: Tick(-10),
                max_tick: Tick(10),
            },
            last_reconciled_block: 1,
        };
        let mut pinned = state("pool", POOL_500, 500, 10, 5);
        pinned.liquidity = legacy.liquidity.0.to_string();
        pinned.coverage_min_tick = 0;
        pinned.coverage_max_tick = 10;
        pinned.state_hash = pinned.canonical_hash().unwrap();
        for direction in [Direction::ZeroForOne, Direction::OneForZero] {
            let existing =
                simulate_current_range_exact_input(&legacy, Amount(100), direction, 10).unwrap();
            let hunter = simulate_pool_exact_input(&pinned, 100, direction, 4).unwrap();
            assert_eq!(hunter.amount_out, existing.amount_out.0);
            assert_eq!(hunter.ticks_crossed, 0);
        }
    }

    #[test]
    fn event_automatically_materializes_one_idempotent_shadow_candidate() {
        let bounds = HunterBounds::default();
        let mut core = HunterCore::new(
            HunterMode::DryRun,
            graph(),
            bounds,
            HunterEconomicConfig {
                flash_premium_bps: 5,
                gas_cost: 1,
                tick_crossing_gas_cost: 1,
                ordering_cost_reserve: 0,
                model_error_reserve_bps: 10,
                shadow_maximum_input: 10_000_000_000_000_000,
            },
        )
        .unwrap();
        let mut states = BTreeMap::new();
        states.insert(
            POOL_500.to_string(),
            agreement(state("uniswap-v3-weth-usdc-500", POOL_500, 500, 10, 0)),
        );
        states.insert(
            POOL_3000.to_string(),
            agreement(state(
                "uniswap-v3-weth-usdc-3000",
                POOL_3000,
                3000,
                60,
                -300,
            )),
        );
        let event = HunterEvent {
            origin_event_id: "phoenix.engine.input.v1:48379269:fixture".to_string(),
            origin_router: "0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45".to_string(),
            chain_id: 42_161,
            block_number: 48_379_269,
            block_hash: format!("0x{}", "a".repeat(64)),
            observed_at_unix_ms: 1_784_878_802_000,
            evaluated_at_unix_ms: 1_784_878_802_000,
            touched_pool_addresses: vec![POOL_500.to_string()],
        };
        let bindings = CandidateBindings {
            risk_snapshot_hash: "f97f050be11ca15357191f946521b272167de5dc116bb2f86f1d417e220c3801"
                .to_string(),
            submission_quote_hash:
                "7ba2937db0288a2f9e82447b0958c6f455592b86a4fe0cb6deac7540fe92002c".to_string(),
            executor_address: "0x17a27f2a51983b574756c2e151ada767e7d54635".to_string(),
            executor_code_hash: "7457a6963c32510f8714d6de4f9291e8b4394933c11db186b5e82c0e681ec697"
                .to_string(),
            submission_channel: "standard_rpc".to_string(),
        };
        let mut sink = InMemoryCandidateSink::default();
        let result = core
            .process_event(&event, &states, &bindings, &mut sink)
            .unwrap();
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(sink.len(), 1);
        let candidate = &result.candidates[0];
        assert_eq!(candidate["schema_version"], CANDIDATE_SCHEMA);
        assert_eq!(candidate["status"], "materialized");
        assert_eq!(candidate["chain_id"], 42_161);
        assert_eq!(candidate["candidate_hash"].as_str().unwrap().len(), 64);
        assert!(
            candidate["conservative_predicted_net_pnl"]
                .as_str()
                .unwrap()
                .parse::<i128>()
                .unwrap()
                > 0
        );
        let committed: Value = serde_json::from_str(include_str!(
            "../../../fixtures/hunter-a1/v1/autonomous-candidate.json"
        ))
        .unwrap();
        assert_eq!(*candidate, committed);
        let duplicate = core
            .process_event(&event, &states, &bindings, &mut sink)
            .unwrap();
        assert!(duplicate.candidates.is_empty());
        let mut stale = event.clone();
        stale.origin_event_id = "phoenix.engine.input.v1:48379269:stale".to_string();
        stale.evaluated_at_unix_ms = stale.observed_at_unix_ms + 2_001;
        let stale_result = core
            .process_event(&stale, &states, &bindings, &mut sink)
            .unwrap();
        assert!(stale_result.candidates.is_empty());
        assert_eq!(stale_result.metrics.candidates_rejected_by_economics, 1);
        assert_eq!(sink.len(), 1);
    }

    #[test]
    fn bounds_have_no_live_or_submission_mode() {
        assert_eq!(HunterMode::Shadow, HunterMode::Shadow);
        assert_eq!(HunterMode::DryRun, HunterMode::DryRun);
        let source = include_str!("mod.rs");
        let live_variant = ["HunterMode::", "Live"].concat();
        let private_signer = ["signer", "_private"].concat();
        let raw_submission = ["send", "_raw_transaction"].concat();
        assert!(!source.contains(&live_variant));
        assert!(!source.contains(&private_signer));
        assert!(!source.contains(&raw_submission));
    }
}
