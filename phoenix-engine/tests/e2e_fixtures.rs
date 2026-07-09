use phoenix_engine::domain::*;
use phoenix_engine::execution::{
    ExecutionCoordinator, ExecutionDecision, ExecutionMode, Opportunity,
};
use phoenix_engine::graph::{PoolEdge, PoolGraph, Route};
use phoenix_engine::optimizer::{optimize, CandidateEvaluation, OptimizerConfig};
use phoenix_engine::origin::{OriginClassification, OriginDetector};
use phoenix_engine::profit::{ProfitInput, ProfitModel};

fn address(hex: &str) -> Address {
    Address::parse(hex).unwrap()
}

fn slot_address(a: &str) -> String {
    format!("000000000000000000000000{}", a.trim_start_matches("0x"))
}

fn slot_u(v: u128) -> String {
    format!("{v:064x}")
}

fn fixture_tx(
    to: Address,
    token_in: &str,
    token_out: &str,
    amount: u128,
) -> phoenix_engine::messaging::NormalizedTx {
    let calldata = format!(
        "0x414bf389{}{}{}{}{}{}{}{}",
        slot_address(token_in),
        slot_address(token_out),
        slot_u(500),
        slot_address("0x1111111111111111111111111111111111111111"),
        slot_u(amount),
        slot_u(0),
        slot_u(0),
        slot_u(0)
    );
    phoenix_engine::messaging::NormalizedTx {
        sequence: SequenceNumber(1),
        tx_hash: TxHash(
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        ),
        tx_type: "0x2".to_string(),
        chain_id: ChainId(42161),
        from: address("0x1111111111111111111111111111111111111111"),
        to: Some(to),
        nonce: 1,
        value: "0".to_string(),
        calldata,
        gas_limit: "300000".to_string(),
        max_fee_per_gas: "100".to_string(),
        max_priority_fee_per_gas: "1".to_string(),
    }
}

fn graph_for(pool_id: PoolId) -> PoolGraph {
    let usdc = TokenAddress(address("0xaf88d065e77c8cc2239327c5edb3a432268e5831"));
    let weth = TokenAddress(address("0x82af49447d8a07e3bd95bd0d56f35241523fbab1"));
    let mut graph = PoolGraph::new();
    graph.add_two_pool_cycle(Route {
        route_id: RouteId("usdc-weth-uni-sushi".to_string()),
        legs: vec![
            PoolEdge {
                pool_id: pool_id.clone(),
                protocol: "UniswapV3".to_string(),
                fee: 500,
                token_in: usdc.clone(),
                token_out: weth.clone(),
                direction: Direction::ZeroForOne,
            },
            PoolEdge {
                pool_id: PoolId("sushi-pool".to_string()),
                protocol: "SushiSwapV3".to_string(),
                fee: 500,
                token_in: weth,
                token_out: usdc,
                direction: Direction::OneForZero,
            },
        ],
    });
    graph
}

#[test]
fn profitable_fixture_reaches_shadow_sink_and_dynamic_sizing() {
    let router = address("0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45");
    let detector = OriginDetector::new(vec![router.clone()]);
    let tx = fixture_tx(
        router,
        "0x82af49447d8a07e3bd95bd0d56f35241523fbab1",
        "0xaf88d065e77c8cc2239327c5edb3a432268e5831",
        500,
    );

    let event = match detector.classify(&tx) {
        OriginClassification::SupportedSwapOrigin(event) => event,
        other => panic!("unexpected origin classification: {other:?}"),
    };
    let touched = event.candidate_touched_pools[0].clone();
    let graph = graph_for(touched.clone());
    let routes = graph.affected_routes(&touched);
    assert_eq!(routes.len(), 1);

    let model = ProfitModel;
    let optimized = optimize(
        OptimizerConfig {
            min_amount: Amount(100),
            max_amount: Amount(900),
            max_evaluations: 25,
            min_profit: Amount(10),
        },
        |amount| {
            let distance = amount.0.abs_diff(500);
            let final_out = 1_100u128.saturating_sub(distance);
            let breakdown = model.evaluate(ProfitInput {
                final_route_output: Amount(final_out),
                principal: amount,
                flash_premium: Amount(1),
                expected_execution_cost: Amount(2),
                expected_ordering_cost: Amount(0),
                uncertainty_reserve: Amount(1),
            })?;
            Ok(CandidateEvaluation {
                amount,
                gross_profit: breakdown.gross_profit,
                flash_premium: Amount(1),
                expected_execution_cost: Amount(2),
                expected_ordering_cost: Amount(0),
                uncertainty_reserve: Amount(1),
                expected_net_profit: breakdown.expected_net_profit,
            })
        },
    )
    .unwrap()
    .unwrap();

    assert_eq!(optimized.best_amount, Amount(500));
    assert_ne!(optimized.best_amount, Amount(100));
    assert!(optimized.expected_net_profit > Amount(10));

    let opportunity = Opportunity {
        opportunity_id: OpportunityId("op-1".to_string()),
        route_id: routes[0].route_id.clone(),
        origin_tx_hash: event.origin_tx_hash.0,
        origin_sequence: event.origin_sequence.0,
        snapshot_id: "snapshot-1".to_string(),
        flash_asset: "USDC".to_string(),
        optimized_amount: optimized.best_amount,
        expected_gross_profit: optimized.gross_profit,
        expected_flash_premium: optimized.flash_premium,
        expected_execution_cost: optimized.expected_execution_cost,
        expected_net_profit: optimized.expected_net_profit,
        exact_ordered_legs: routes[0].legs.clone(),
        min_profit: Amount(10),
        expires_at_unix_ms: 1_700_000_001_000,
        created_at_monotonic_ns: 1,
        simulation_latency_ns: 1,
    };
    let coordinator = ExecutionCoordinator::new(ExecutionMode::Shadow);
    assert_eq!(
        coordinator.submit(&opportunity),
        ExecutionDecision::RecordedShadow
    );
}

#[test]
fn unsupported_router_fixture_is_measured_not_guessed() {
    let router = address("0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45");
    let detector = OriginDetector::new(vec![router]);
    let tx = fixture_tx(
        address("0x9999999999999999999999999999999999999999"),
        "0x82af49447d8a07e3bd95bd0d56f35241523fbab1",
        "0xaf88d065e77c8cc2239327c5edb3a432268e5831",
        500,
    );
    assert_eq!(detector.classify(&tx), OriginClassification::PossibleAggregator);
}

#[test]
fn non_profitable_fixture_does_not_create_opportunity() {
    let result = optimize(
        OptimizerConfig {
            min_amount: Amount(100),
            max_amount: Amount(300),
            max_evaluations: 10,
            min_profit: Amount(10),
        },
        |amount| {
            Ok(CandidateEvaluation {
                amount,
                gross_profit: Amount(1),
                flash_premium: Amount(1),
                expected_execution_cost: Amount(1),
                expected_ordering_cost: Amount(0),
                uncertainty_reserve: Amount(0),
                expected_net_profit: Amount(0),
            })
        },
    )
    .unwrap();
    assert!(result.is_none());
}
