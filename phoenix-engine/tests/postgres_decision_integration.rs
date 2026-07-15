use chrono::Utc;
use phoenix_engine::domain::{
    Address, Amount, OpportunityId, PoolId, RouteId, TokenAddress, TxHash,
};
use phoenix_engine::engine_input::{EngineClassification, InputIdentity};
use phoenix_engine::graph::PoolEdge;
use phoenix_engine::opportunity::{
    AgreementState, BasisPoints, CostBreakdown, DecisionEvidence, IndependentVerificationStatus,
    MarketEvidence, Opportunity, OpportunityIdentity, OutcomeEvidence, PoolStateEvidence,
    PrimaryProfitabilityStatus, RejectionReason, RiskFlag, RouteEvidence, ScenarioEconomics,
    ShadowDisposition, SignedAmount, SimulationClassification, SimulationEvidence, SimulationKind,
    StateSource, Strategy, VerificationSkipReason, VerificationStatus, PROFITABILITY_MODEL_VERSION,
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
        include_str!("../../migrations/006_dependency_exhaustion_quarantine.sql"),
        include_str!("../../migrations/007_canonical_profitability_truth.sql"),
        include_str!("../../migrations/008_shadow_route_discovery_indexes.sql"),
        include_str!("../../migrations/009_profit_triggered_secondary_verification.sql"),
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
        gross_profit: SignedAmount(95),
        pool_fees: Amount(5),
        estimated_execution_gas: 1,
        gas_price_wei: 1,
        arbitrum_execution_fee: Amount(1),
        total_cost: Amount(6),
        expected_net_pnl: SignedAmount(94),
        expected_roi_bps: BasisPoints(9_400),
        probability_of_success_bps: 8_000,
        expected_value_after_success_probability: SignedAmount(75),
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
                route_config_hash: Some("f".repeat(64)),
                quote_block: 100,
                quote_age_ms: 1,
                state_source: StateSource::BlockPinnedRpc,
                primary_provider_id: Some("provider_0".to_string()),
                primary_response_hash: Some("d".repeat(64)),
                primary_state_hash: Some("e".repeat(64)),
                secondary_provider_id: None,
                secondary_state_hash: None,
                secondary_block_number: None,
                secondary_block_hash: None,
                secondary_route_config_hash: None,
                verification_status: VerificationStatus::PrimaryOnly,
                independent_verification_status: IndependentVerificationStatus::NotRequested,
                independent_verification_lifecycle: vec![
                    IndependentVerificationStatus::NotRequested,
                ],
                agreement_state: AgreementState::NotChecked,
                verification_skip_reason: Some(
                    VerificationSkipReason::PrimaryScreenNoProfitableCandidate,
                ),
                feed_to_detection_latency_ns: 1,
            },
            economics: ScenarioEconomics {
                base: economics.clone(),
                conservative: economics.clone(),
                severe: economics,
                minimum_required_net_pnl: SignedAmount(100),
                primary_status: PrimaryProfitabilityStatus::BelowMinimum,
                model_version: PROFITABILITY_MODEL_VERSION.to_string(),
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
                simulated_net_pnl: Some(SignedAmount(94)),
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
                shadow_only: true,
                execution_eligible: false,
                execution_request_created: false,
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
        "TRUNCATE shadow_profitability_facts, rpc_quality_records, shadow_decisions, \
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
    (SELECT count(*) FROM shadow_profitability_facts) AS profitability_facts,
    (SELECT count(*) FROM shadow_profitability_facts
        WHERE evidence_completeness_status = 'complete') AS complete_facts,
    (SELECT count(*) FROM rpc_quality_records) AS quality,
    (SELECT count(*) FROM shadow_decisions WHERE execution_eligible) AS executable,
    (SELECT count(*) FROM shadow_profitability_facts
        WHERE NOT shadow_only OR execution_eligible OR execution_request_created) AS unsafe_facts
"#,
    )
    .fetch_one(&pool)
    .await
    .expect("load Engine integration counts");
    assert_eq!(counts.try_get::<i64, _>("classifications").unwrap(), 2);
    assert_eq!(counts.try_get::<i64, _>("decisions").unwrap(), 2);
    assert_eq!(counts.try_get::<i64, _>("profitability_facts").unwrap(), 2);
    assert_eq!(counts.try_get::<i64, _>("complete_facts").unwrap(), 2);
    assert_eq!(counts.try_get::<i64, _>("quality").unwrap(), 2);
    assert_eq!(counts.try_get::<i64, _>("executable").unwrap(), 0);
    assert_eq!(counts.try_get::<i64, _>("unsafe_facts").unwrap(), 0);

    let canonical = sqlx::query(
        r#"
SELECT gross_spread::text AS gross_spread,
       gross_profit::text AS gross_profit,
       total_cost::text AS total_cost,
       expected_net_pnl::text AS expected_net_pnl,
       minimum_required_net_pnl::text AS minimum_required_net_pnl,
       primary_profitability_status,
       verification_status,
       route_config_hash,
       independent_verification_status,
       independent_verification_lifecycle,
       verification_skip_reason
FROM shadow_profitability_facts
WHERE shadow_decision_id = CAST($1 AS uuid)
"#,
    )
    .bind("11111111-1111-8111-8111-111111111111")
    .fetch_one(&pool)
    .await
    .expect("load canonical profitability fact");
    assert_eq!(
        canonical.try_get::<String, _>("gross_spread").unwrap(),
        "100"
    );
    assert_eq!(
        canonical.try_get::<String, _>("gross_profit").unwrap(),
        "95"
    );
    assert_eq!(canonical.try_get::<String, _>("total_cost").unwrap(), "6");
    assert_eq!(
        canonical.try_get::<String, _>("expected_net_pnl").unwrap(),
        "94"
    );
    assert_eq!(
        canonical
            .try_get::<String, _>("minimum_required_net_pnl")
            .unwrap(),
        "100"
    );
    assert_eq!(
        canonical
            .try_get::<String, _>("primary_profitability_status")
            .unwrap(),
        "below_minimum"
    );
    assert_eq!(
        canonical
            .try_get::<String, _>("verification_status")
            .unwrap(),
        "primary_only"
    );
    assert_eq!(
        canonical.try_get::<String, _>("route_config_hash").unwrap(),
        "f".repeat(64)
    );
    assert_eq!(
        canonical
            .try_get::<String, _>("independent_verification_status")
            .unwrap(),
        "not_requested"
    );
    assert_eq!(
        canonical
            .try_get::<serde_json::Value, _>("independent_verification_lifecycle")
            .unwrap(),
        json!(["not_requested"])
    );
    assert_eq!(
        canonical
            .try_get::<String, _>("verification_skip_reason")
            .unwrap(),
        "primary_screen_no_profitable_candidate"
    );

    let mut plan_transaction = pool.begin().await.expect("begin query-plan check");
    sqlx::query("SET LOCAL enable_seqscan = off")
        .execute(&mut *plan_transaction)
        .await
        .expect("prefer index for query-plan check");
    let plan_rows = sqlx::query(
        "EXPLAIN (COSTS OFF) SELECT shadow_decision_id FROM shadow_profitability_facts \
         ORDER BY evaluated_at DESC, shadow_decision_id DESC LIMIT 100",
    )
    .fetch_all(&mut *plan_transaction)
    .await
    .expect("explain bounded profitability query");
    let plan = plan_rows
        .iter()
        .map(|row| row.try_get::<String, _>(0).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        plan.contains("shadow_profitability_evaluated_idx"),
        "{plan}"
    );
    plan_transaction
        .rollback()
        .await
        .expect("rollback query-plan check");

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
    let rolled_back_fact: i64 = sqlx::query(
        "SELECT count(*) AS count FROM shadow_profitability_facts \
         WHERE shadow_decision_id = CAST($1 AS uuid)",
    )
    .bind("33333333-3333-8333-8333-333333333333")
    .fetch_one(&pool)
    .await
    .expect("verify canonical fact rollback")
    .try_get("count")
    .unwrap();
    assert_eq!(rolled_back_fact, 0);

    sqlx::raw_sql(
        r#"
DROP TRIGGER phoenix_test_reject_rpc_quality_trigger ON rpc_quality_records;
DROP FUNCTION phoenix_test_reject_rpc_quality();
"#,
    )
    .execute(&pool)
    .await
    .expect("remove Engine integration rollback trigger");

    let mut transient = record('4', "44444444-4444-8444-8444-444444444444");
    transient.classification = EngineClassification::TransientDependencyFailure;
    transient.detail_class = Some("rpc_gateway_unavailable");
    transient.decision_count = 0;
    transient.evaluations.clear();
    transient.evidence = json!({
        "route_fingerprints": ["two-pool-v1"],
        "dependency_failure_class": "rpc_gateway_unavailable"
    });
    assert_eq!(
        store.persist_classification(&transient).await,
        Ok(PersistOutcome::Committed)
    );
    let context = store
        .dependency_failure_context(&transient.identity.source_event_identity)
        .await
        .expect("load first dependency failure")
        .expect("dependency failure context exists");
    assert_eq!(
        context.classification,
        EngineClassification::TransientDependencyFailure
    );
    assert_eq!(
        context.detail_class.as_deref(),
        Some("rpc_gateway_unavailable")
    );
    assert_eq!(context.delivery_attempt, 1);
    assert_eq!(
        context.evidence["dependency_failure_class"],
        "rpc_gateway_unavailable"
    );
    let incomplete = sqlx::query(
        "SELECT evidence_completeness_status, expected_net_pnl::text AS expected_net_pnl \
         FROM shadow_profitability_report_rows \
         WHERE source_event_identity = $1 AND route_fingerprint = 'two-pool-v1'",
    )
    .bind(&transient.identity.source_event_identity)
    .fetch_one(&pool)
    .await
    .expect("load incomplete candidate reporting row");
    assert_eq!(
        incomplete
            .try_get::<String, _>("evidence_completeness_status")
            .unwrap(),
        "incomplete"
    );
    assert_eq!(
        incomplete
            .try_get::<Option<String>, _>("expected_net_pnl")
            .unwrap(),
        None
    );
}
