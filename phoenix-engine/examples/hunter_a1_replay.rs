use phoenix_engine::amm::v3::sqrt_ratio_at_tick;
use phoenix_engine::hunter::{
    CandidateBindings, HunterBounds, HunterCore, HunterEconomicConfig, HunterEvent, HunterMode,
    HunterRouteGraph, InMemoryCandidateSink,
};
use rpc_gateway::hunter_state::{
    PinnedV3PoolState, ProviderStateAgreement, PINNED_V3_STATE_SCHEMA,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::Instant;

const WETH: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
const USDC: &str = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";
const POOL_500: &str = "0xc6962004f452be9203591991d15f6b388e09e8d0";
const POOL_3000: &str = "0xc473e2aee3441bf9240be85eb122abb059a3b57c";
const FACTORY: &str = "0x1f98431c8ad98523631ae4a59f267346ea31f984";
const BLOCK: u64 = 48_379_269;

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
        block_number: BLOCK,
        block_hash: format!("0x{}", "a".repeat(64)),
        pool_id: pool_id.to_string(),
        pool_address: pool_address.to_string(),
        pool_code_hash: "b".repeat(64),
        factory_address: FACTORY.to_string(),
        protocol_id: "uniswap-v3".to_string(),
        token0: WETH.to_string(),
        token1: USDC.to_string(),
        fee,
        tick_spacing: spacing,
        sqrt_price_x96: sqrt_ratio_at_tick(tick).expect("fixture tick").to_string(),
        tick,
        liquidity: "1000000000000000000000000000000".to_string(),
        coverage_min_tick: tick - spacing * 4,
        coverage_max_tick: tick + spacing * 4,
        tick_bitmap_words: Vec::new(),
        initialized_ticks: Vec::new(),
        state_hash: "0".repeat(64),
    };
    value.state_hash = value.canonical_hash().expect("fixture state hash");
    value
}

fn agreement(state: PinnedV3PoolState) -> ProviderStateAgreement {
    ProviderStateAgreement {
        primary_provider_id: "fixture-primary".to_string(),
        secondary_provider_id: "fixture-secondary".to_string(),
        primary: state.clone(),
        secondary: state,
    }
}

fn main() {
    let bounds = HunterBounds::default();
    let graph = HunterRouteGraph::from_contracts(
        include_str!("../../config/phoenix-route-universe-v1.json"),
        &[include_str!("../../config/phoenix-route-policy-v1.json")],
        bounds,
    )
    .expect("fixture graph");
    let summary = graph.summary().clone();
    let mut core = HunterCore::new(
        HunterMode::DryRun,
        graph,
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
    .expect("fixture core");
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
    let bindings = CandidateBindings {
        risk_snapshot_hash: "f97f050be11ca15357191f946521b272167de5dc116bb2f86f1d417e220c3801"
            .to_string(),
        submission_quote_hash: "7ba2937db0288a2f9e82447b0958c6f455592b86a4fe0cb6deac7540fe92002c"
            .to_string(),
        executor_address: "0x17a27f2a51983b574756c2e151ada767e7d54635".to_string(),
        executor_code_hash: "7457a6963c32510f8714d6de4f9291e8b4394933c11db186b5e82c0e681ec697"
            .to_string(),
        submission_channel: "standard_rpc".to_string(),
    };
    let event = HunterEvent {
        origin_event_id: "phoenix.engine.input.v1:48379269:fixture".to_string(),
        origin_router: "0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45".to_string(),
        chain_id: 42_161,
        block_number: BLOCK,
        block_hash: format!("0x{}", "a".repeat(64)),
        observed_at_unix_ms: 1_784_878_802_000,
        evaluated_at_unix_ms: 1_784_878_802_000,
        touched_pool_addresses: vec![POOL_500.to_string()],
    };
    let mut sink = InMemoryCandidateSink::default();
    let started = Instant::now();
    let profitable = core
        .process_event(&event, &states, &bindings, &mut sink)
        .expect("profitable fixture event");
    let profitable_latency = started.elapsed().as_nanos();

    let duplicate_started = Instant::now();
    let duplicate = core
        .process_event(&event, &states, &bindings, &mut sink)
        .expect("duplicate fixture event");
    let duplicate_latency = duplicate_started.elapsed().as_nanos();

    let mut unmatched_event = event.clone();
    unmatched_event.origin_event_id = "phoenix.engine.input.v1:48379270:fixture".to_string();
    unmatched_event.touched_pool_addresses =
        vec!["0x9999999999999999999999999999999999999999".to_string()];
    let unmatched_started = Instant::now();
    let unmatched = core
        .process_event(&unmatched_event, &states, &bindings, &mut sink)
        .expect("unmatched fixture event");
    let unmatched_latency = unmatched_started.elapsed().as_nanos();

    let candidate = profitable
        .candidates
        .first()
        .expect("one profitable fixture candidate");
    let mut latencies = [profitable_latency, duplicate_latency, unmatched_latency];
    latencies.sort();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "schema_version": "phoenix.hunter-a1-replay-evidence.v1",
            "evidence_class": "deterministic_fixture_replay",
            "baseline_routes": 1,
            "enumerable_routes": summary.enumerable_route_count,
            "shadow_enabled_routes": summary.shadow_enabled_route_count,
            "baseline_pools": 2,
            "reviewed_pools": summary.pool_count,
            "new_reviewed_pools": 0,
            "events_processed": 3,
            "affected_routes_evaluated": profitable.metrics.routes_evaluated,
            "qualified_candidates": sink.len(),
            "candidate_rate_bps": 3333,
            "positive_conservative_net_pnl": [
                candidate["conservative_predicted_net_pnl"]
            ],
            "positive_conservative_net_pnl_p50": candidate["conservative_predicted_net_pnl"],
            "positive_conservative_net_pnl_p95": candidate["conservative_predicted_net_pnl"],
            "evaluation_latency_ns": [
                profitable_latency.to_string(),
                duplicate_latency.to_string(),
                unmatched_latency.to_string()
            ],
            "evaluation_latency_p50_ns": latencies[1].to_string(),
            "evaluation_latency_p95_ns": latencies[2].to_string(),
            "rpc_state_reads_per_event": [2, 0, 0],
            "state_incomplete_rate_bps": 0,
            "prediction_vs_fork_simulation_error_bps": Value::Null,
            "prediction_error_note": "not claimed by this deterministic replay; exact cross-tick parity is covered by the committed offline pinned-state fixture",
            "duplicate_candidate_outputs": duplicate.candidates.len(),
            "unmatched_candidate_outputs": unmatched.candidates.len(),
            "candidate": candidate
        }))
        .expect("serialize fixture evidence")
    );
}
