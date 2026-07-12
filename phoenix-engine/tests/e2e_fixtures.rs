use phoenix_engine::domain::*;
use phoenix_engine::execution::{
    ExecutionCoordinator, ExecutionDecision, ExecutionMode, Opportunity,
};
use phoenix_engine::graph::{PoolEdge, PoolGraph, Route};
use phoenix_engine::opportunity::{
    BasisPoints, CostBreakdown, DecisionEvidence, MarketEvidence, OpportunityIdentity,
    OutcomeEvidence, PoolStateEvidence, RouteEvidence, ScenarioEconomics, ShadowDisposition,
    SignedAmount, SimulationClassification, SimulationEvidence, SimulationKind, StateSource,
    Strategy,
};
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
            let synthetic_profit = 1_000u128.saturating_sub(distance.saturating_mul(2));
            let final_out = amount.0.saturating_add(synthetic_profit);
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

    let base = CostBreakdown {
        gross_spread: SignedAmount(optimized.gross_profit.0 as i128),
        flash_loan_fee: optimized.flash_premium,
        arbitrum_execution_fee: optimized.expected_execution_cost,
        uncertainty_reserve: optimized.uncertainty_reserve,
        expected_net_pnl: SignedAmount(optimized.expected_net_profit.0 as i128),
        expected_roi_bps: BasisPoints(100),
        probability_of_success_bps: 9_000,
        expected_value_after_success_probability: SignedAmount(
            optimized.expected_net_profit.0 as i128,
        ),
        ..CostBreakdown::default()
    };
    let opportunity = Opportunity {
        identity: OpportunityIdentity {
            opportunity_id: OpportunityId("op-1".to_string()),
            strategy: Strategy::TwoPoolV3Arbitrage,
            strategy_version: "fixture-v1".to_string(),
            detector_version: "fixture-v1".to_string(),
            code_version: "test".to_string(),
            config_version: "test".to_string(),
            chain_id: 42161,
            source_sequence: event.origin_sequence.0,
            origin_tx_hash: event.origin_tx_hash,
            observed_block: 1,
            observed_at_unix_ms: 1_700_000_000_000,
            detected_at_unix_ms: 1_700_000_000_001,
        },
        route: RouteEvidence {
            route_id: routes[0].route_id.clone(),
            route_fingerprint: "fixture-route-v1".to_string(),
            token_path: vec![
                routes[0].legs[0].token_in.clone(),
                routes[0].legs[1].token_in.clone(),
                routes[0].legs[1].token_out.clone(),
            ],
            pools: routes[0]
                .legs
                .iter()
                .map(|leg| leg.pool_id.clone())
                .collect(),
            protocols: routes[0]
                .legs
                .iter()
                .map(|leg| leg.protocol.clone())
                .collect(),
            input_token: routes[0].legs[0].token_in.clone(),
            output_token: routes[0].legs[1].token_out.clone(),
            input_amount: optimized.best_amount,
            expected_output: Amount(optimized.best_amount.0 + optimized.gross_profit.0),
            exact_ordered_legs: routes[0].legs.clone(),
        },
        market: MarketEvidence {
            pool_states: vec![PoolStateEvidence {
                pool: touched,
                state_hash: "fixture-state-hash".to_string(),
                reserve_or_liquidity_summary: "fixture-only".to_string(),
            }],
            state_block: 1,
            state_block_hash: Some("fixture-block-hash".to_string()),
            quote_block: 1,
            quote_age_ms: 1,
            state_source: StateSource::RecordedCheckpoint,
            rpc_provider_id: None,
            rpc_response_hash: None,
            feed_to_detection_latency_ns: 1,
        },
        economics: ScenarioEconomics {
            base: base.clone(),
            conservative: base.clone(),
            severe: base,
        },
        simulation: SimulationEvidence {
            kind: SimulationKind::HistoricalReplay,
            block_number: 1,
            block_hash: Some("fixture-block-hash".to_string()),
            from_address: None,
            target_contract: None,
            contract_code_hash: None,
            calldata_hash: "fixture-calldata-hash".to_string(),
            value: Amount::ZERO,
            gas_estimate: Some(1),
            gas_used: Some(1),
            simulated_output: Some(optimized.best_amount),
            simulated_net_pnl: Some(SignedAmount(optimized.expected_net_profit.0 as i128)),
            revert_reason: None,
            state_overrides_hash: None,
            provider_id: None,
            simulated_at_unix_ms: 1_700_000_000_002,
            latency_ns: 1,
            state_drift_bps: BasisPoints(0),
            classification: SimulationClassification::Passed,
        },
        decision: DecisionEvidence {
            disposition: ShadowDisposition::Accepted,
            primary_rejection_reason: None,
            secondary_rejection_reasons: Vec::new(),
            risk_flags: Vec::new(),
            confidence_bps: 9_000,
            policy_version: "fixture-policy-v1".to_string(),
            execution_eligible: false,
            decided_at_unix_ms: 1_700_000_000_003,
        },
        outcome: OutcomeEvidence {
            opportunity_expires_at_unix_ms: 1_700_000_001_000,
            ..OutcomeEvidence::default()
        },
    };
    opportunity.validate_traceability().unwrap();
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
    assert_eq!(
        detector.classify(&tx),
        OriginClassification::PossibleAggregator
    );
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
