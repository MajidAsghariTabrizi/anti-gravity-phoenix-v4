use chrono::Utc;
use phoenix_engine::domain::{
    Address, Amount, OpportunityId, PoolId, RouteId, TokenAddress, TxHash,
};
use phoenix_engine::engine_input::{EngineClassification, InputIdentity};
use phoenix_engine::graph::PoolEdge;
use phoenix_engine::opportunity::{
    BasisPoints, CostBreakdown, DecisionEvidence, MarketEvidence, Opportunity, OpportunityIdentity,
    OutcomeEvidence, PoolStateEvidence, RejectionReason, RiskFlag, RouteEvidence,
    ScenarioEconomics, ShadowDisposition, SignedAmount, SimulationClassification,
    SimulationEvidence, SimulationKind, StateSource, Strategy,
};
use phoenix_engine::persistence::{
    ClassificationRecord, PersistOutcome, PostgresShadowStore, ShadowStore, StoreError,
};
use phoenix_engine::shadow_processor::EvaluatedOpportunity;
use phoenix_engine::Direction;
use rpc_gateway::shadow_state::RpcQualityEvidence;
use serde_json::json;
use sqlx::{PgPool, Row};

const BLOCK_HASH: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn local_postgres_dsn() -> Option<String> {
    let dsn = std::env::var("PHOENIX_TEST_POSTGRES_DSN").ok()?;
    assert!(
        dsn.contains("@127.0.0.1:") || dsn.contains("@localhost:"),
        "integration test PostgreSQL URL must be loopback-only"
    );
    Some(dsn)
}

async fn apply_migrations(pool: &PgPool) {
    for migration in [
        include_str!("../../migrations/001_init.sql"),
        include_str!("../../migrations/002_event_signatures.sql"),
        include_str!("../../migrations/003_shadow_profitability_evidence.sql"),
        include_str!("../../migrations/004_shadow_engine_runtime.sql"),
        include_str!("../../migrations/005_shadow_decision_identity.sql"),
    ] {
        sqlx::raw_sql(migration)
            .execute(pool)
            .await
            .expect("apply Engine integration migration");
    }
}

fn token(value: &str) -> TokenAddress {
    TokenAddress(Address::parse(value).expect("canonical integration token"))
}

fn evaluation(hash_byte: char, opportunity_id: &str) -> EvaluatedOpportunity {
    let token0 = token("0x1111111111111111111111111111111111111111");
    let token1 = token("0x2222222222222222222222222222222222222222");
    let first_pool = PoolId("origin-pool".to_string());
    let second_pool = PoolId("comparison-pool".to_string());
    let economics = CostBreakdown {
        gross_spread: SignedAmount(100),
        pool_fees: Amount(5),
        estimated_execution_gas: 1,
        gas_price_wei: 1,
        expected_net_pnl: SignedAmount(50),
        expected_roi_bps: BasisPoints(500),
        probability_of_success_bps: 8_000,
        expected_value_after_success_probability: SignedAmount(40),
        ..CostBreakdown::default()
    };
    let tx_hash = format!("0x{}", hash_byte.to_string().repeat(64));
    EvaluatedOpportunity {
        opportunity: Opportunity {
            identity: OpportunityIdentity {
                opportunity_id: OpportunityId(opportunity_id.to_string()),
                strategy: Strategy::TwoPoolV3Arbitrage,
                strategy_version: "two-pool-v3-block-state-v1".to_string(),
                detector_version: "exact-input-single-v1".to_string(),
                code_version: "integration-test".to_string(),
                config_version: "two-pool-v1".to_string(),
                chain_id: 42161,
                source_sequence: 7,
                origin_tx_hash: TxHash(tx_hash),
                observed_block: 100,
                observed_at_unix_ms: 1_700_000_000_000,
                detected_at_unix_ms: 1_700_000_000_001,
            },
            route: RouteEvidence {
                route_id: RouteId("two-pool".to_string()),
                route_fingerprint: "two-pool-v1".to_string(),
                token_path: vec![token0.clone(), token1.clone(), token0.clone()],
                pools: vec![first_pool.clone(), second_pool.clone()],
                protocols: vec!["UniswapV3".to_string(), "SushiSwapV3".to_string()],
                input_token: token0.clone(),
                output_token: token0.clone(),
                input_amount: Amount(100),
                expected_output: Amount(200),
                exact_ordered_legs: vec![
                    PoolEdge {
                        pool_id: first_pool.clone(),
                        protocol: "UniswapV3".to_string(),
                        fee: 500,
                        token_in: token0.clone(),
                        token_out: token1.clone(),
                        direction: Direction::ZeroForOne,
                    },
                    PoolEdge {
                        pool_id: second_pool.clone(),
                        protocol: "SushiSwapV3".to_string(),
                        fee: 500,
                        token_in: token1,
                        token_out: token0,
                        direction: Direction::OneForZero,
                    },
                ],
            },
            market: MarketEvidence {
                pool_states: vec![
                    PoolStateEvidence {
                        pool: first_pool,
                        state_hash: "b".repeat(64),
                        reserve_or_liquidity_summary: "tick=1;liquidity=100".to_string(),
                    },
                    PoolStateEvidence {
                        pool: second_pool,
                        state_hash: "c".repeat(64),
                        reserve_or_liquidity_summary: "tick=2;liquidity=200".to_string(),
                    },
                ],
                state_block: 100,
                state_block_hash: Some(BLOCK_HASH.to_string()),
                quote_block: 100,
                quote_age_ms: 1,
                state_source: StateSource::BlockPinnedRpc,
                rpc_provider_id: Some("provider_0".to_string()),
                rpc_response_hash: Some("d".repeat(64)),
                feed_to_detection_latency_ns: 1,
            },
            economics: ScenarioEconomics {
                base: economics.clone(),
                conservative: economics.clone(),
                severe: economics,
            },
            simulation: SimulationEvidence {
                kind: SimulationKind::StateBased,
                block_number: 100,
                block_hash: Some(BLOCK_HASH.to_string()),
                from_address: None,
                target_contract: None,
                contract_code_hash: None,
                calldata_hash: "e".repeat(64),
                value: Amount::ZERO,
                gas_estimate: Some(1),
                gas_used: None,
                simulated_output: Some(Amount(200)),
                simulated_net_pnl: Some(SignedAmount(50)),
                revert_reason: None,
                state_overrides_hash: None,
                provider_id: Some("provider_0".to_string()),
                simulated_at_unix_ms: 1_700_000_000_001,
                latency_ns: 1,
                state_drift_bps: BasisPoints(0),
                classification: SimulationClassification::NotRun,
            },
            decision: DecisionEvidence {
                disposition: ShadowDisposition::Rejected,
                primary_rejection_reason: Some(RejectionReason::LiquidityInsufficient),
                secondary_rejection_reasons: vec![RejectionReason::SimulationEvidenceInsufficient],
                risk_flags: vec![RiskFlag::IncompleteLiquidity],
                confidence_bps: 7_000,
                policy_version: "shadow-state-policy-v1".to_string(),
                execution_eligible: false,
                decided_at_unix_ms: 1_700_000_000_002,
            },
            outcome: OutcomeEvidence {
                opportunity_expires_at_unix_ms: 1_700_000_002_000,
                ..OutcomeEvidence::default()
            },
        },
        rpc_quality: vec![RpcQualityEvidence {
            provider_id: "provider_0".to_string(),
            method: "eth_call".to_string(),
            block_number: Some(100),
            block_hash: Some(BLOCK_HASH.to_string()),
            response_hash: Some("f".repeat(64)),
            latency_ns: 100,
            success: true,
            stale_result: false,
            disagreement: false,
            timeout: false,
            retry_count: 0,
        }],
    }
}

fn record(hash_byte: char, opportunity_id: &str) -> ClassificationRecord {
    let now = Utc::now();
    let tx_hash = format!("0x{}", hash_byte.to_string().repeat(64));
    ClassificationRecord {
        identity: InputIdentity {
            source_event_identity: format!("phoenix.engine.input.v1:7:{tx_hash}"),
            source_sequence: 7,
            tx_hash,
            chain_id: 42161,
        },
        classification: EngineClassification::CandidateRejected,
        detail_class: Some("shadow_policy_rejected"),
        candidate_count: 1,
        decision_count: 1,
        delivery_attempt: 1,
        evidence: json!({"evaluation": "block_pinned_rpc_state"}),
        first_received_at: now,
        completed_at: now,
        processing_latency_ns: 100,
        evaluations: vec![evaluation(hash_byte, opportunity_id)],
    }
}

#[tokio::test]
async fn full_decision_commit_is_atomic_idempotent_and_source_scoped() {
    let Some(dsn) = local_postgres_dsn() else {
        return;
    };
    let pool = PgPool::connect(&dsn)
        .await
        .expect("connect Engine integration PostgreSQL");
    apply_migrations(&pool).await;
    sqlx::query(
        "TRUNCATE rpc_quality_records, shadow_decisions, \
         shadow_engine_processing_attempts, shadow_engine_classifications CASCADE",
    )
    .execute(&pool)
    .await
    .expect("reset Engine integration tables");

    let store = PostgresShadowStore::connect(&dsn, "disable")
        .await
        .expect("connect Engine shadow store");
    store.verify_schema().await.expect("verify Engine schema");

    let first = record('1', "11111111-1111-8111-8111-111111111111");
    assert_eq!(
        store.persist_classification(&first).await,
        Ok(PersistOutcome::Committed)
    );
    assert_eq!(
        store
            .final_classification(&first.identity.source_event_identity)
            .await,
        Ok(Some(EngineClassification::CandidateRejected))
    );
    assert_eq!(
        store.persist_classification(&first).await,
        Ok(PersistOutcome::AlreadyFinal)
    );

    let second = record('2', "22222222-2222-8222-8222-222222222222");
    assert_eq!(
        store.persist_classification(&second).await,
        Ok(PersistOutcome::Committed)
    );
    let counts = sqlx::query(
        r#"
SELECT
    (SELECT count(*) FROM shadow_engine_classifications) AS classifications,
    (SELECT count(*) FROM shadow_decisions) AS decisions,
    (SELECT count(*) FROM rpc_quality_records) AS quality,
    (SELECT count(*) FROM shadow_decisions WHERE execution_eligible) AS executable
"#,
    )
    .fetch_one(&pool)
    .await
    .expect("load Engine integration counts");
    assert_eq!(counts.try_get::<i64, _>("classifications").unwrap(), 2);
    assert_eq!(counts.try_get::<i64, _>("decisions").unwrap(), 2);
    assert_eq!(counts.try_get::<i64, _>("quality").unwrap(), 2);
    assert_eq!(counts.try_get::<i64, _>("executable").unwrap(), 0);

    sqlx::raw_sql(
        r#"
CREATE OR REPLACE FUNCTION phoenix_test_reject_rpc_quality() RETURNS trigger AS $$
BEGIN
    IF NEW.provider_id = 'forced_failure' THEN
        RAISE EXCEPTION 'forced Engine integration rollback';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER phoenix_test_reject_rpc_quality_trigger
BEFORE INSERT ON rpc_quality_records
FOR EACH ROW EXECUTE FUNCTION phoenix_test_reject_rpc_quality();
"#,
    )
    .execute(&pool)
    .await
    .expect("install Engine integration rollback trigger");
    let mut rejected = record('3', "33333333-3333-8333-8333-333333333333");
    rejected.evaluations[0].rpc_quality[0].provider_id = "forced_failure".to_string();
    assert_eq!(
        store.persist_classification(&rejected).await,
        Err(StoreError::Transaction)
    );
    let rolled_back: i64 = sqlx::query(
        "SELECT count(*) AS count FROM shadow_engine_classifications \
         WHERE source_event_identity = $1",
    )
    .bind(&rejected.identity.source_event_identity)
    .fetch_one(&pool)
    .await
    .expect("verify Engine integration rollback")
    .try_get("count")
    .unwrap();
    assert_eq!(rolled_back, 0);

    sqlx::raw_sql(
        r#"
DROP TRIGGER phoenix_test_reject_rpc_quality_trigger ON rpc_quality_records;
DROP FUNCTION phoenix_test_reject_rpc_quality();
"#,
    )
    .execute(&pool)
    .await
    .expect("remove Engine integration rollback trigger");
}
