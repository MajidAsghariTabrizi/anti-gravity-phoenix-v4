use chrono::Utc;
use phoenix_fork_sandbox::model::{
    CounterfactualResult, CounterfactualResultBody, ForkIdentity, PinnedBlockEvidence,
    SimulationEvidence, SimulationStatus,
};
use phoenix_fork_sandbox::{ForkEvidenceStore, PlanPolicy, StoreError, UnsignedPlanner};
use rpc_gateway::shadow_state::{
    EvidenceRequest, PoolStateRequest, ShadowStateRequest, SHADOW_STATE_SCHEMA_VERSION,
};
use sqlx::{PgPool, Row};
use std::collections::BTreeSet;

const DECISION_ID: &str = "11111111-1111-8111-8111-111111111111";
const BLOCK_HASH: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TOKEN_A: &str = "0x1111111111111111111111111111111111111111";
const TOKEN_B: &str = "0x2222222222222222222222222222222222222222";
const POOL_A: &str = "0x3333333333333333333333333333333333333333";
const POOL_B: &str = "0x4444444444444444444444444444444444444444";
const ROUTER: &str = "0x5555555555555555555555555555555555555555";
const TARGET: &str = "0x6666666666666666666666666666666666666666";
const SIMULATION_FROM: &str = "0x7777777777777777777777777777777777777777";

#[tokio::test]
async fn versioned_fact_load_and_append_only_result_are_atomic() {
    let Some(dsn) = std::env::var("PHOENIX_TEST_POSTGRES_DSN").ok() else {
        return;
    };
    let pool = PgPool::connect(&dsn)
        .await
        .expect("connect fork test database");
    apply_migrations(&pool).await;
    sqlx::query(
        "TRUNCATE fork_simulation_results, shadow_profitability_facts, shadow_decisions CASCADE",
    )
    .execute(&pool)
    .await
    .expect("reset fork evidence tables");
    insert_decision(&pool).await;
    let route_hash = route_hash();
    insert_fact(&pool, &route_hash).await;

    let store = ForkEvidenceStore::connect(&dsn, "disable")
        .await
        .expect("connect fork evidence store");
    let fact = store
        .load_opportunity(DECISION_ID)
        .await
        .expect("load versioned fork fact");
    assert_eq!(fact.route_config_hash, route_hash);
    assert_eq!(fact.pool_address_path, vec![POOL_A, POOL_B]);
    let now = Utc::now();
    let plan = UnsignedPlanner
        .build(
            &fact,
            &policy(),
            u64::try_from(now.timestamp_millis()).expect("positive timestamp"),
        )
        .expect("build plan from database fact");
    let result = CounterfactualResult::from_body(CounterfactualResultBody {
        schema_version: "phoenix.fork-result.v1".to_string(),
        plan_hash: plan.canonical_hash().expect("plan hash"),
        shadow_decision_id: DECISION_ID.to_string(),
        status: SimulationStatus::Reverted,
        predicted_gross_profit: "100".to_string(),
        predicted_total_cost: "10".to_string(),
        predicted_net_pnl: "90".to_string(),
        simulated_gross_profit: None,
        simulated_gas_cost: None,
        simulated_balance_delta: None,
        simulated_net_pnl: None,
        prediction_error: None,
        gas_estimate: None,
        gas_used: None,
        model_version: fact.model_version,
        policy_version: fact.policy_version,
        fork: ForkIdentity {
            chain_id: 42161,
            fork_block: PinnedBlockEvidence {
                number: 100,
                hash: BLOCK_HASH.to_string(),
            },
            fork_instance_hash: "e".repeat(64),
            local_block: PinnedBlockEvidence {
                number: 100,
                hash: BLOCK_HASH.to_string(),
            },
        },
        simulated_at: now,
        revert_reason: Some("MinProfit".to_string()),
        evidence: SimulationEvidence {
            rpc_methods: vec!["anvil_metadata".to_string(), "eth_estimateGas".to_string()],
            target_code_hash: plan.target_code_hash.clone(),
            observed_pool_state_hashes: plan.pool_state_hash_path.clone(),
            observed_aggregate_state_hash: plan.primary_state_hash.clone(),
            call_output_hash: None,
            trace_hash: None,
            settled_route_hash: None,
        },
        fork_only: true,
        shadow_only: true,
        live_execution: false,
        execution_eligible: false,
        execution_request_created: false,
        public_broadcast: false,
        signer_used: false,
    })
    .expect("build reverted result");
    let mut tampered = result.clone();
    tampered.result_hash = "a".repeat(64);
    assert_eq!(
        store.persist_result(&plan, &tampered).await,
        Err(StoreError::Integrity)
    );
    store
        .persist_result(&plan, &result)
        .await
        .expect("persist fork result");
    assert_eq!(
        store.persist_result(&plan, &result).await,
        Err(StoreError::Integrity)
    );
    let row = sqlx::query(
        "SELECT count(*) AS count,
                bool_and(fork_only AND shadow_only AND NOT live_execution
                    AND NOT execution_eligible AND NOT execution_request_created
                    AND NOT public_broadcast AND NOT signer_used) AS safe
           FROM fork_simulation_results",
    )
    .fetch_one(&pool)
    .await
    .expect("read fork result safety");
    assert_eq!(row.try_get::<i64, _>("count").expect("result count"), 1);
    assert!(row.try_get::<bool, _>("safe").expect("result safety"));
}

async fn apply_migrations(pool: &PgPool) {
    for migration in [
        include_str!("../../migrations/001_init.sql"),
        include_str!("../../migrations/002_event_signatures.sql"),
        include_str!("../../migrations/003_shadow_profitability_evidence.sql"),
        include_str!("../../migrations/004_shadow_engine_runtime.sql"),
        include_str!("../../migrations/005_shadow_decision_identity.sql"),
        include_str!("../../migrations/006_dependency_exhaustion_quarantine.sql"),
        include_str!("../../migrations/007_canonical_profitability_truth.sql"),
        include_str!("../../migrations/008_shadow_route_discovery_indexes.sql"),
        include_str!("../../migrations/009_profit_triggered_secondary_verification.sql"),
        include_str!("../../migrations/010_fork_simulation_evidence.sql"),
    ] {
        sqlx::raw_sql(migration)
            .execute(pool)
            .await
            .expect("apply fork integration migration");
    }
}

async fn insert_decision(pool: &PgPool) {
    sqlx::query(
        r#"
INSERT INTO shadow_decisions (
    id, strategy, strategy_version, detector_version, code_version,
    config_version, policy_version, chain_id, source_sequence,
    observed_block, state_block, quote_block, route_fingerprint,
    disposition, confidence_bps, execution_eligible, base_net_pnl,
    conservative_net_pnl, severe_net_pnl, identity_evidence,
    route_evidence, market_evidence, economics_evidence,
    simulation_evidence, decision_evidence, outcome_evidence,
    observed_at, detected_at, decided_at, source_event_identity,
    secondary_rejection_reasons, risk_flags, processing_latency_ns
) VALUES (
    CAST($1 AS uuid), 'two_pool_v3_arbitrage', 'fixture-v1', 'fixture-v1',
    'integration-test', 'fixture-v1', 'shadow-state-policy-v1', 42161,
    7, 100, 100, 100, 'fixture-route-v1', 'accepted', 9500, false,
    90, 80, 70, '{}'::jsonb, '{}'::jsonb, '{}'::jsonb, '{}'::jsonb,
    '{}'::jsonb, '{}'::jsonb, '{}'::jsonb, now() - interval '1 second',
    now() - interval '1 second', now(), $2, '[]'::jsonb, '[]'::jsonb, 1
)
"#,
    )
    .bind(DECISION_ID)
    .bind(format!("phoenix.engine.input.v1:7:0x{}", "9".repeat(64)))
    .execute(pool)
    .await
    .expect("insert fork integration decision");
}

async fn insert_fact(pool: &PgPool, route_hash: &str) {
    sqlx::query(
        r#"
INSERT INTO shadow_profitability_facts (
    shadow_decision_id, source_event_identity, source_sequence,
    transaction_hash, origin_router, chain_id, route_id,
    route_fingerprint, detected_at, evaluated_at, pinned_block_number,
    pinned_block_hash, primary_state_hash, token_path, pool_path,
    fee_path, pool_address_path, protocol_path, direction_path,
    expected_leg_outputs, pool_state_hash_path, opportunity_expires_at,
    fork_evidence_schema_version, input_amount, expected_output,
    gross_spread, gross_profit, dex_fees, price_impact, execution_gas,
    gas_price, arbitrum_execution_fee, l1_data_fee, flash_loan_premium,
    protocol_fees, failed_attempt_reserve, ordering_reserve,
    slippage_reserve, stale_state_reserve, state_drift_reserve,
    latency_reserve, uncertainty_reserve, contract_overhead, total_cost,
    expected_net_pnl, conservative_net_pnl, severe_net_pnl,
    minimum_required_net_pnl, primary_profitability_status, disposition,
    secondary_rejection_reasons, model_version, policy_version,
    detector_version, code_version, primary_provider_id,
    primary_response_hash, route_config_hash, secondary_provider_id,
    secondary_state_hash, secondary_block_number, secondary_block_hash,
    secondary_route_config_hash, verification_status,
    independent_verification_status, independent_verification_lifecycle,
    agreement_state, shadow_only, execution_eligible,
    execution_request_created, evidence_completeness_status
) VALUES (
    CAST($1 AS uuid), $2, 7, $3, $4, 42161, 'fixture-route',
    'fixture-route-v1', now() - interval '1 second', now(), 100, $5,
    $6, $7::jsonb, $8::jsonb, $9::jsonb, $10::jsonb, $11::jsonb,
    $12::jsonb, $13::jsonb, $14::jsonb, now() + interval '1 hour',
    'phoenix.fork-evidence.v1', 100, 200, 100, 100, 0, 0, 10, 1, 10,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 90, 80, 70, 50,
    'meets_minimum', 'accepted', '[]'::jsonb, 'shadow-profitability-v1',
    'shadow-state-policy-v1', 'fixture-v1', 'integration-test',
    'provider_0', $15, $16, 'provider_1', $6, 100, $5, $16, 'agreed',
    'agreed', '["requested", "agreed"]'::jsonb, 'agreed', true, false,
    false, 'complete'
)
"#,
    )
    .bind(DECISION_ID)
    .bind(format!("phoenix.engine.input.v1:7:0x{}", "9".repeat(64)))
    .bind(format!("0x{}", "9".repeat(64)))
    .bind(ROUTER)
    .bind(BLOCK_HASH)
    .bind("e".repeat(64))
    .bind(format!(r#"["{TOKEN_A}","{TOKEN_B}","{TOKEN_A}"]"#))
    .bind(r#"["pool-a","pool-b"]"#)
    .bind("[500,3000]")
    .bind(format!(r#"["{POOL_A}","{POOL_B}"]"#))
    .bind(r#"["UniswapV3","SushiSwapV3"]"#)
    .bind(r#"["zero_for_one","one_for_zero"]"#)
    .bind(r#"["150","200"]"#)
    .bind(format!(r#"["{}","{}"]"#, "b".repeat(64), "c".repeat(64)))
    .bind("d".repeat(64))
    .bind(route_hash)
    .execute(pool)
    .await
    .expect("insert fork integration profitability fact");
}

fn route_hash() -> String {
    ShadowStateRequest {
        schema_version: SHADOW_STATE_SCHEMA_VERSION.to_string(),
        chain_id: 42161,
        route_fingerprint: "fixture-route-v1".to_string(),
        pools: vec![
            PoolStateRequest {
                pool_id: "pool-a".to_string(),
                address: POOL_A.to_string(),
                protocol: "UniswapV3".to_string(),
                token0: TOKEN_A.to_string(),
                token1: TOKEN_B.to_string(),
                fee: 500,
            },
            PoolStateRequest {
                pool_id: "pool-b".to_string(),
                address: POOL_B.to_string(),
                protocol: "SushiSwapV3".to_string(),
                token0: TOKEN_A.to_string(),
                token1: TOKEN_B.to_string(),
                fee: 3_000,
            },
        ],
        evidence: EvidenceRequest::Primary,
    }
    .route_config_hash()
    .expect("fork integration route hash")
}

fn policy() -> PlanPolicy {
    PlanPolicy {
        allowed_tokens: [TOKEN_A.to_string(), TOKEN_B.to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>(),
        allowed_pools: [POOL_A.to_string(), POOL_B.to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>(),
        allowed_routers: [ROUTER.to_string()].into_iter().collect(),
        allowed_protocols: ["UniswapV3".to_string(), "SushiSwapV3".to_string()]
            .into_iter()
            .collect(),
        target_contract: TARGET.to_string(),
        target_code_hash: "f".repeat(64),
        simulation_from: SIMULATION_FROM.to_string(),
        minimum_net_pnl: 50,
        maximum_input_amount: 1_000,
        slippage_bps: 100,
        maximum_calldata_bytes: 65_536,
    }
}
