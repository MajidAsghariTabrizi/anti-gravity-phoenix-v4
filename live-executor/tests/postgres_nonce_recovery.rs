use chrono::{Duration as ChronoDuration, Utc};
use phoenix_fork_sandbox::model::{
    CounterfactualResult, CounterfactualResultBody, ForkIdentity, PinnedBlockEvidence,
    SimulationEvidence, SimulationStatus,
};
use phoenix_fork_sandbox::{ForkEvidenceStore, PlanPolicy, UnsignedPlanner};
use phoenix_live_executor::abi::encode_execute_opportunity;
use phoenix_live_executor::approval::{ApprovalInput, ApprovalMaterializer};
use phoenix_live_executor::config::{ExecutorConfig, SafetyLimits};
use phoenix_live_executor::model::{
    AttemptStatus, CanonicalAddress, ExecutionRequest, ReceiptOutcome, Settlement, TransactionHash,
    ValidatedLeg,
};
use phoenix_live_executor::signer::TransactionSigner;
use phoenix_live_executor::store::{ExecutorStore, PostgresExecutorStore};
use phoenix_live_executor::{
    ARBITRUM_NATIVE_USDC_ADDRESS, ARBITRUM_ONE_CHAIN_ID, ARBITRUM_WETH_ADDRESS,
    CURRENT_ROUTE_FINGERPRINT, CURRENT_ROUTE_POOL_3000_ADDRESS, CURRENT_ROUTE_POOL_500_ADDRESS,
    REQUEST_SCHEMA_VERSION,
};
use rpc_gateway::shadow_state::{
    EvidenceRequest, PoolStateRequest, ShadowStateRequest, SHADOW_STATE_SCHEMA_VERSION,
};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::collections::BTreeSet;
use std::time::Duration;
use url::Url;
use uuid::Uuid;

const APPROVAL_DECISION_ID: &str = "11111111-1111-8111-8111-111111111111";
const APPROVAL_BLOCK_HASH: &str =
    "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const APPROVAL_TOKEN_B: &str = ARBITRUM_NATIVE_USDC_ADDRESS;
const APPROVAL_POOL_A: &str = CURRENT_ROUTE_POOL_500_ADDRESS;
const APPROVAL_POOL_B: &str = CURRENT_ROUTE_POOL_3000_ADDRESS;
const APPROVAL_POOL_A_ID: &str =
    "0x82af49447d8a07e3bd95bd0d56f35241523fbab1:0xaf88d065e77c8cc2239327c5edb3a432268e5831:500";
const APPROVAL_POOL_B_ID: &str =
    "0x82af49447d8a07e3bd95bd0d56f35241523fbab1:0xaf88d065e77c8cc2239327c5edb3a432268e5831:3000";
const APPROVAL_ROUTER: &str = "0x5555555555555555555555555555555555555555";
const APPROVAL_EXECUTOR: &str = "0x6666666666666666666666666666666666666666";
const APPROVAL_SIMULATION_FROM: &str = "0x7777777777777777777777777777777777777777";

#[tokio::test]
async fn nonce_allocation_and_pending_state_survive_restart() {
    let Some(dsn) = std::env::var("PHOENIX_TEST_POSTGRES_DSN").ok() else {
        eprintln!("PHOENIX_TEST_POSTGRES_DSN is unset; skipping PostgreSQL integration");
        return;
    };
    let pool = PgPool::connect(&dsn).await.expect("connect test database");
    sqlx::raw_sql("DROP SCHEMA IF EXISTS live_canary CASCADE")
        .execute(&pool)
        .await
        .expect("reset service schema");
    sqlx::raw_sql(include_str!("../schema/001_live_canary.sql"))
        .execute(&pool)
        .await
        .expect("apply service schema");
    sqlx::raw_sql(include_str!("../schema/002_approval_evidence.sql"))
        .execute(&pool)
        .await
        .expect("apply approval evidence schema");
    sqlx::raw_sql(include_str!(
        "../schema/003_autonomous_hunter_contracts.sql"
    ))
    .execute(&pool)
    .await
    .expect("apply autonomous Hunter contract schema");

    let signer = TransactionSigner::from_secret(&hex::encode([13_u8; 32]), ARBITRUM_ONE_CHAIN_ID)
        .expect("signer");
    let config = test_config(&dsn, signer.address());
    let store = PostgresExecutorStore::from_pool(pool.clone());
    store.validate_schema().await.expect("schema");
    sqlx::query(
        "UPDATE live_canary.control
         SET armed = true, kill_switch = false, disarm_reason = 'test_armed'
         WHERE singleton",
    )
    .execute(&pool)
    .await
    .expect("arm isolated database");

    let now = Utc::now();
    let first = request(Uuid::from_u128(10), now, config.pnl_asset_address);
    insert_approved(&pool, &first).await;
    assert!(
        sqlx::query(
            "UPDATE live_canary.execution_requests
             SET executor_code_hash = NULL
             WHERE id = $1",
        )
        .bind(first.id)
        .execute(&pool)
        .await
        .is_err(),
        "v2 approval evidence fields must not become null"
    );
    let mut duplicate = first.clone();
    duplicate.id = Uuid::from_u128(999);
    duplicate.approval_digest = duplicate
        .canonical_approval_digest()
        .expect("duplicate digest");
    assert!(
        try_insert_approved(&pool, &duplicate).await.is_err(),
        "an opportunity cannot be approved under a second request id"
    );
    let claimed = store
        .claim_approved(&config, now)
        .await
        .expect("claim")
        .expect("approved request");
    assert_eq!(claimed.id, first.id);
    assert_eq!(
        store
            .claim_approved(&config, now)
            .await
            .expect("second claim"),
        None,
        "one active canary attempt must block every later request"
    );
    let first_nonce = store
        .allocate_nonce(first.id, &config, 5)
        .await
        .expect("allocate first nonce");
    assert_eq!(first_nonce, 5);

    let restarted = PostgresExecutorStore::from_pool(pool.clone());
    let recovered = restarted
        .active_attempt()
        .await
        .expect("recover")
        .expect("durable active attempt");
    assert_eq!(recovered.status, AttemptStatus::NonceAllocated);
    assert_eq!(recovered.nonce, Some(5));
    assert_eq!(recovered.tx_hash, None);
    restarted
        .mark_submission_unknown(first.id, "isolated_restart_recovery", now)
        .await
        .expect("preserve unknown submission");
    let unknown_restart = PostgresExecutorStore::from_pool(pool.clone());
    let unknown = unknown_restart
        .active_attempt()
        .await
        .expect("recover unknown submission")
        .expect("unknown submission remains active");
    assert_eq!(unknown.status, AttemptStatus::SubmissionUnknown);
    assert_eq!(unknown.nonce, Some(5));
    assert_eq!(
        unknown_restart
            .claim_approved(&config, now)
            .await
            .expect("unknown blocks claim"),
        None
    );

    sqlx::query(
        "UPDATE live_canary.execution_attempts
         SET status = 'failed', terminal_at = $2, updated_at = $2
         WHERE request_id = $1 AND status = 'submission_unknown'",
    )
    .bind(first.id)
    .bind(now)
    .execute(&pool)
    .await
    .expect("fixture operator resolves unknown attempt");
    sqlx::query(
        "UPDATE live_canary.execution_requests
         SET status = 'failed', updated_at = $2
         WHERE id = $1 AND status = 'submission_unknown'",
    )
    .bind(first.id)
    .bind(now)
    .execute(&pool)
    .await
    .expect("fixture operator resolves unknown request");

    let second = request(Uuid::from_u128(11), now, config.pnl_asset_address);
    insert_approved(&pool, &second).await;
    restarted
        .claim_approved(&config, now)
        .await
        .expect("claim second")
        .expect("second request");
    let second_nonce = restarted
        .allocate_nonce(second.id, &config, 3)
        .await
        .expect("allocate durable next nonce");
    assert_eq!(
        second_nonce, 6,
        "database nonce must not regress to the lower RPC pending nonce"
    );
    let tx_hash = TransactionHash::from_bytes([15_u8; 32]);
    restarted
        .mark_pending(second.id, tx_hash, now)
        .await
        .expect("persist hash");

    let second_restart = PostgresExecutorStore::from_pool(pool.clone());
    let pending = second_restart
        .active_attempt()
        .await
        .expect("recover pending")
        .expect("pending attempt");
    assert_eq!(pending.status, AttemptStatus::Pending);
    assert_eq!(pending.nonce, Some(6));
    assert_eq!(pending.tx_hash, Some(tx_hash));

    let nonce_row = sqlx::query(
        "SELECT next_nonce::text AS next_nonce
         FROM live_canary.nonce_state
         WHERE chain_id = 42161 AND wallet_address = $1",
    )
    .bind(config.wallet_address.to_string())
    .fetch_one(&pool)
    .await
    .expect("nonce row");
    assert_eq!(
        nonce_row
            .try_get::<String, _>("next_nonce")
            .expect("next nonce"),
        "7"
    );

    second_restart
        .mark_terminal(
            second.id,
            AttemptStatus::TimedOut,
            Some("isolated_cleanup"),
            None,
            now,
        )
        .await
        .expect("close pending attempt");

    let timed_out = second_restart
        .active_attempt()
        .await
        .expect("recover timed-out attempt")
        .expect("timed-out hash remains active");
    assert_eq!(timed_out.status, AttemptStatus::TimedOut);
    assert_eq!(
        second_restart
            .claim_approved(&config, now)
            .await
            .expect("blocked claim"),
        None,
        "a timed-out hash must continue to block a second canary"
    );
    let confirmed_outcome = ReceiptOutcome {
        receipt_status: 1,
        settled_event_found: true,
        block_number: 100,
        gas_used: 10,
        effective_gas_price: 10,
        actual_fee_wei: 100,
        settlement: Settlement {
            asset: config.pnl_asset_address,
            flash_amount: second.flash_amount,
            premium: 1,
            realized_profit: 500,
        },
        net_pnl_wei: 400,
    };
    second_restart
        .mark_terminal(
            second.id,
            AttemptStatus::Confirmed,
            None,
            Some(&confirmed_outcome),
            now,
        )
        .await
        .expect("reconcile late receipt");

    let third = request(Uuid::from_u128(12), now, config.pnl_asset_address);
    insert_approved(&pool, &third).await;
    second_restart
        .claim_approved(&config, now)
        .await
        .expect("claim third")
        .expect("third request");
    second_restart
        .allocate_nonce(third.id, &config, 7)
        .await
        .expect("allocate third nonce");
    let reverted_hash = TransactionHash::from_bytes([16_u8; 32]);
    second_restart
        .mark_pending(third.id, reverted_hash, now)
        .await
        .expect("persist reverted hash");
    let reverted_outcome = ReceiptOutcome {
        receipt_status: 0,
        settled_event_found: false,
        block_number: 101,
        gas_used: 10,
        effective_gas_price: 10,
        actual_fee_wei: 100,
        settlement: Settlement {
            asset: config.pnl_asset_address,
            flash_amount: third.flash_amount,
            premium: 0,
            realized_profit: 0,
        },
        net_pnl_wei: -100,
    };
    second_restart
        .mark_terminal(
            third.id,
            AttemptStatus::Reverted,
            Some("transaction_reverted"),
            Some(&reverted_outcome),
            now,
        )
        .await
        .expect("persist reverted gas loss");
    assert_eq!(
        second_restart
            .daily_loss_wei(now)
            .await
            .expect("daily loss"),
        100
    );

    let fourth = request(Uuid::from_u128(13), now, config.pnl_asset_address);
    insert_approved(&pool, &fourth).await;
    second_restart
        .claim_approved(&config, now)
        .await
        .expect("claim fourth")
        .expect("fourth request");
    assert_eq!(
        second_restart
            .allocate_nonce(fourth.id, &config, 8)
            .await
            .expect("allocate fourth nonce"),
        8
    );
    second_restart
        .fail_unsubmitted(fourth.id, "isolated_pre_submit_cancel", now)
        .await
        .expect("release unsubmitted nonce");

    let fifth = request(Uuid::from_u128(14), now, config.pnl_asset_address);
    insert_approved(&pool, &fifth).await;
    second_restart
        .claim_approved(&config, now)
        .await
        .expect("claim fifth")
        .expect("fifth request");
    assert_eq!(
        second_restart
            .allocate_nonce(fifth.id, &config, 8)
            .await
            .expect("reuse released nonce"),
        8
    );
    second_restart
        .fail_unsubmitted(fifth.id, "isolated_cleanup", now)
        .await
        .expect("close final fixture attempt");

    prepare_fork_approval_fixture(&pool).await;
    sqlx::query(
        "UPDATE live_canary.control
         SET armed = false, kill_switch = true, disarm_reason = 'approval_fixture'
         WHERE singleton",
    )
    .execute(&pool)
    .await
    .expect("engage approval fixture kill switch");
    let plan = approval_plan(&dsn).await;
    let result = approval_result(&plan, now);
    let fork_store = ForkEvidenceStore::connect(&dsn, "disable")
        .await
        .expect("connect fork result store");
    fork_store
        .persist_result(&plan, &result)
        .await
        .expect("persist independently simulated result");
    let materializer = ApprovalMaterializer::from_pool(pool.clone());
    let approval = ApprovalInput {
        simulation_result_hash: result.result_hash.clone(),
        approved_by: "postgres-approval-fixture".to_string(),
        approval_ttl_seconds: 300,
        max_priority_fee_per_gas: 1,
    };
    let created = materializer
        .materialize(approval.clone(), now)
        .await
        .expect("materialize approved request");
    assert!(created.created);
    let replayed = materializer
        .materialize(approval, now + ChronoDuration::seconds(1))
        .await
        .expect("idempotent materializer replay");
    assert!(!replayed.created);
    assert_eq!(replayed.request_id, created.request_id);
    let approved_count: i64 = sqlx::query_scalar(
        "SELECT count(*)
         FROM live_canary.execution_requests
         WHERE simulation_result_hash = $1
           AND plan_hash = $2
           AND status = 'approved'",
    )
    .bind(&result.result_hash)
    .bind(plan.canonical_hash().expect("plan hash"))
    .fetch_one(&pool)
    .await
    .expect("count materialized approval");
    assert_eq!(approved_count, 1);

    sqlx::raw_sql("DROP SCHEMA live_canary CASCADE")
        .execute(&pool)
        .await
        .expect("drop service schema");
}

async fn prepare_fork_approval_fixture(pool: &PgPool) {
    let fork_table: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('public.fork_simulation_results')::text")
            .fetch_one(pool)
            .await
            .expect("inspect fork result schema");
    if fork_table.is_none() {
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
                .expect("apply fork approval fixture migration");
        }
    }
    sqlx::query(
        "TRUNCATE fork_simulation_results, shadow_profitability_facts, shadow_decisions CASCADE",
    )
    .execute(pool)
    .await
    .expect("reset fork approval fixture tables");
    insert_approval_decision(pool).await;
    insert_approval_fact(pool, &approval_route_hash()).await;
}

async fn approval_plan(dsn: &str) -> phoenix_fork_sandbox::model::UnsignedTransactionPlan {
    let store = ForkEvidenceStore::connect(dsn, "disable")
        .await
        .expect("connect fork approval fixture");
    let fact = store
        .load_opportunity(APPROVAL_DECISION_ID)
        .await
        .expect("load approval profitability fact");
    UnsignedPlanner
        .build(
            &fact,
            &approval_policy(),
            u64::try_from(Utc::now().timestamp_millis()).expect("positive timestamp"),
        )
        .expect("build approval plan")
}

fn approval_result(
    plan: &phoenix_fork_sandbox::model::UnsignedTransactionPlan,
    now: chrono::DateTime<Utc>,
) -> CounterfactualResult {
    CounterfactualResult::from_body(CounterfactualResultBody {
        schema_version: phoenix_fork_sandbox::model::RESULT_SCHEMA_VERSION.to_string(),
        plan_hash: plan.canonical_hash().expect("approval plan hash"),
        shadow_decision_id: plan.shadow_decision_id.clone(),
        status: SimulationStatus::Passed,
        predicted_gross_profit: plan.predicted.gross_profit.clone(),
        predicted_total_cost: plan.predicted.total_cost.clone(),
        predicted_net_pnl: plan.predicted.net_pnl.clone(),
        simulated_gross_profit: Some("100".to_string()),
        simulated_gas_cost: Some("5".to_string()),
        simulated_balance_delta: Some("100".to_string()),
        simulated_net_pnl: Some("95".to_string()),
        prediction_error: Some("5".to_string()),
        gas_estimate: Some(10),
        gas_used: Some(5),
        model_version: plan.model_version.clone(),
        policy_version: plan.policy_version.clone(),
        fork: ForkIdentity {
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            fork_block: plan.pinned_block.clone(),
            fork_instance_hash: "3".repeat(64),
            local_block: PinnedBlockEvidence {
                number: plan.pinned_block.number + 1,
                hash: format!("0x{}", "4".repeat(64)),
            },
        },
        simulated_at: now,
        revert_reason: None,
        evidence: SimulationEvidence {
            rpc_methods: vec!["eth_call".to_string(), "debug_traceCall".to_string()],
            target_code_hash: plan.target_code_hash.clone(),
            observed_pool_state_hashes: plan.pool_state_hash_path.clone(),
            observed_aggregate_state_hash: plan.primary_state_hash.clone(),
            call_output_hash: Some("1".repeat(64)),
            trace_hash: Some("2".repeat(64)),
            settled_route_hash: Some(plan.route_hash.clone()),
        },
        fork_only: true,
        shadow_only: true,
        live_execution: false,
        execution_eligible: false,
        execution_request_created: false,
        public_broadcast: false,
        signer_used: false,
    })
    .expect("build approval result")
}

async fn insert_approval_decision(pool: &PgPool) {
    sqlx::query(
        r#"
INSERT INTO shadow_decisions (
    id, strategy, strategy_version, detector_version, code_version,
    config_version, policy_version, chain_id, source_sequence,
    observed_block, state_block, quote_block, route_fingerprint,
    disposition, primary_rejection_reason, confidence_bps, execution_eligible, base_net_pnl,
    conservative_net_pnl, severe_net_pnl, identity_evidence,
    route_evidence, market_evidence, economics_evidence,
    simulation_evidence, decision_evidence, outcome_evidence,
    observed_at, detected_at, decided_at, source_event_identity,
    secondary_rejection_reasons, risk_flags, processing_latency_ns
) VALUES (
    CAST($1 AS uuid), 'two_pool_v3_arbitrage', 'fixture-v1', 'fixture-v1',
    'integration-test', 'fixture-v1', 'shadow-state-policy-v1', 42161,
    7, 100, 100, 100, 'arbitrum-weth-usdc-uniswap-v3-500-3000-v1', 'rejected',
    'contract_path_unavailable', 9500, false,
    90, 80, 70, '{}'::jsonb, '{}'::jsonb, '{}'::jsonb, '{}'::jsonb,
    '{}'::jsonb, '{}'::jsonb, '{}'::jsonb, now() - interval '1 second',
    now() - interval '1 second', now(), $2, '[]'::jsonb, '[]'::jsonb, 1
)
"#,
    )
    .bind(APPROVAL_DECISION_ID)
    .bind(format!("phoenix.engine.input.v1:7:0x{}", "9".repeat(64)))
    .execute(pool)
    .await
    .expect("insert approval decision");
}

async fn insert_approval_fact(pool: &PgPool, route_hash: &str) {
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
    final_rejection_reason, secondary_rejection_reasons, model_version, policy_version,
    detector_version, code_version, primary_provider_id,
    primary_response_hash, route_config_hash, secondary_provider_id,
    secondary_state_hash, secondary_block_number, secondary_block_hash,
    secondary_route_config_hash, verification_status,
    independent_verification_status, independent_verification_lifecycle,
    agreement_state, shadow_only, execution_eligible,
    execution_request_created, evidence_completeness_status
) VALUES (
    CAST($1 AS uuid), $2, 7, $3, $4, 42161, 'fixture-route',
    'arbitrum-weth-usdc-uniswap-v3-500-3000-v1',
    now() - interval '1 second', now(), 100, $5,
    $6, $7::jsonb, $8::jsonb, $9::jsonb, $10::jsonb, $11::jsonb,
    $12::jsonb, $13::jsonb, $14::jsonb, now() + interval '1 hour',
    'phoenix.fork-evidence.v1', 100, 200, 100, 100, 0, 0, 10, 1, 10,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 90, 80, 70, 50,
    'meets_minimum', 'rejected', 'contract_path_unavailable', '[]'::jsonb,
    'shadow-profitability-v1',
    'shadow-state-policy-v1', 'fixture-v1', 'integration-test',
    'provider_0', $15, $16, 'provider_1', $6, 100, $5, $16, 'agreed',
    'agreed', '["requested", "agreed"]'::jsonb, 'agreed', true, false,
    false, 'complete'
)
"#,
    )
    .bind(APPROVAL_DECISION_ID)
    .bind(format!("phoenix.engine.input.v1:7:0x{}", "9".repeat(64)))
    .bind(format!("0x{}", "9".repeat(64)))
    .bind(APPROVAL_ROUTER)
    .bind(APPROVAL_BLOCK_HASH)
    .bind("e".repeat(64))
    .bind(format!(
        r#"["{ARBITRUM_WETH_ADDRESS}","{APPROVAL_TOKEN_B}","{ARBITRUM_WETH_ADDRESS}"]"#
    ))
    .bind(format!(
        r#"["{APPROVAL_POOL_A_ID}","{APPROVAL_POOL_B_ID}"]"#
    ))
    .bind("[500,3000]")
    .bind(format!(r#"["{APPROVAL_POOL_A}","{APPROVAL_POOL_B}"]"#))
    .bind(r#"["UniswapV3","UniswapV3"]"#)
    .bind(r#"["zero_for_one","one_for_zero"]"#)
    .bind(r#"["150","200"]"#)
    .bind(format!(r#"["{}","{}"]"#, "b".repeat(64), "c".repeat(64)))
    .bind("d".repeat(64))
    .bind(route_hash)
    .execute(pool)
    .await
    .expect("insert approval profitability fact");
}

fn approval_route_hash() -> String {
    ShadowStateRequest {
        schema_version: SHADOW_STATE_SCHEMA_VERSION.to_string(),
        chain_id: ARBITRUM_ONE_CHAIN_ID,
        route_fingerprint: CURRENT_ROUTE_FINGERPRINT.to_string(),
        pools: vec![
            PoolStateRequest {
                pool_id: APPROVAL_POOL_A_ID.to_string(),
                address: APPROVAL_POOL_A.to_string(),
                protocol: "UniswapV3".to_string(),
                token0: ARBITRUM_WETH_ADDRESS.to_string(),
                token1: APPROVAL_TOKEN_B.to_string(),
                token0_decimals: 18,
                token1_decimals: 6,
                fee: 500,
                tick_spacing: 10,
            },
            PoolStateRequest {
                pool_id: APPROVAL_POOL_B_ID.to_string(),
                address: APPROVAL_POOL_B.to_string(),
                protocol: "UniswapV3".to_string(),
                token0: ARBITRUM_WETH_ADDRESS.to_string(),
                token1: APPROVAL_TOKEN_B.to_string(),
                token0_decimals: 18,
                token1_decimals: 6,
                fee: 3_000,
                tick_spacing: 60,
            },
        ],
        evidence: EvidenceRequest::Primary,
    }
    .route_config_hash()
    .expect("approval route hash")
}

fn approval_policy() -> PlanPolicy {
    PlanPolicy {
        allowed_tokens: [
            ARBITRUM_WETH_ADDRESS.to_string(),
            APPROVAL_TOKEN_B.to_string(),
        ]
        .into_iter()
        .collect::<BTreeSet<_>>(),
        allowed_pools: [APPROVAL_POOL_A.to_string(), APPROVAL_POOL_B.to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>(),
        allowed_routers: [APPROVAL_ROUTER.to_string()].into_iter().collect(),
        allowed_protocols: ["UniswapV3".to_string()].into_iter().collect(),
        target_contract: APPROVAL_EXECUTOR.to_string(),
        target_code_hash: "f".repeat(64),
        simulation_from: APPROVAL_SIMULATION_FROM.to_string(),
        minimum_net_pnl: 50,
        maximum_input_amount: 1_000,
        slippage_bps: 100,
        maximum_calldata_bytes: 65_536,
    }
}

fn test_config(dsn: &str, wallet_address: CanonicalAddress) -> ExecutorConfig {
    let rpc_url = Url::parse("https://rpc.example.invalid").expect("url");
    ExecutorConfig {
        postgres_dsn: dsn.to_string(),
        rpc_url: rpc_url.clone(),
        rpc_allowlist: vec![rpc_url],
        wallet_address,
        executor_address: CanonicalAddress::parse("0x3333333333333333333333333333333333333333")
            .expect("executor"),
        executor_code_hash: "a".repeat(64),
        pnl_asset_address: CanonicalAddress::parse(ARBITRUM_WETH_ADDRESS).expect("asset"),
        chain_id: ARBITRUM_ONE_CHAIN_ID,
        limits: SafetyLimits {
            maximum_gas_limit: 500_000,
            maximum_max_fee_per_gas: 1_000,
            maximum_priority_fee_per_gas: 100,
            maximum_input_amount: 1_000_000,
            minimum_expected_profit: 100,
            maximum_daily_loss_wei: 1_000_000_000,
        },
        receipt_timeout: Duration::from_secs(90),
        poll_interval: Duration::from_secs(1),
        one_transaction_at_a_time: true,
    }
}

fn request(
    id: Uuid,
    now: chrono::DateTime<Utc>,
    flash_asset: CanonicalAddress,
) -> ExecutionRequest {
    let token_b = CanonicalAddress::parse(ARBITRUM_NATIVE_USDC_ADDRESS).expect("token");
    let mut request = ExecutionRequest {
        id,
        opportunity_id: Uuid::from_u128(id.as_u128() + 100),
        schema_version: REQUEST_SCHEMA_VERSION.to_string(),
        chain_id: ARBITRUM_ONE_CHAIN_ID,
        route_id: [17_u8; 32],
        route_fingerprint: CURRENT_ROUTE_FINGERPRINT.to_string(),
        selected_size: 1_000,
        token_path: vec![flash_asset, token_b, flash_asset],
        origin_router: CanonicalAddress::parse("0x4444444444444444444444444444444444444444")
            .expect("router"),
        executor_address: CanonicalAddress::parse("0x3333333333333333333333333333333333333333")
            .expect("executor"),
        executor_code_hash: "a".repeat(64),
        calldata_hash: String::new(),
        simulation_result_hash: hex::encode(Sha256::digest(format!("result-{id}"))),
        plan_hash: hex::encode(Sha256::digest(format!("plan-{id}"))),
        pinned_block_number: 123_456,
        pinned_block_hash: format!("0x{}", "d".repeat(64)),
        flash_asset,
        flash_amount: 1_000,
        maximum_input_amount: 1_000,
        minimum_profit: 100,
        expected_profit: 500,
        deadline: now + ChronoDuration::minutes(2),
        legs: vec![
            ValidatedLeg {
                pool: CanonicalAddress::parse(CURRENT_ROUTE_POOL_500_ADDRESS).expect("pool"),
                token_in: flash_asset,
                token_out: token_b,
                fee: 500,
                zero_for_one: true,
                min_amount_out: 900,
            },
            ValidatedLeg {
                pool: CanonicalAddress::parse(CURRENT_ROUTE_POOL_3000_ADDRESS).expect("pool"),
                token_in: token_b,
                token_out: flash_asset,
                fee: 3_000,
                zero_for_one: false,
                min_amount_out: 1_100,
            },
        ],
        gas_limit: 400_000,
        max_fee_per_gas: 900,
        max_priority_fee_per_gas: 90,
        approved_by: "isolated-postgres-test".to_string(),
        approved_at: now - ChronoDuration::seconds(1),
        approval_deadline: now + ChronoDuration::minutes(1),
        policy_version: "phoenix.live-canary-approval.v1".to_string(),
        approval_digest: String::new(),
    };
    request.calldata_hash = hex::encode(Sha256::digest(
        encode_execute_opportunity(&request, request.executor_address).expect("calldata"),
    ));
    request.approval_digest = request
        .canonical_approval_digest()
        .expect("approval digest");
    request
}

async fn insert_approved(pool: &PgPool, request: &ExecutionRequest) {
    try_insert_approved(pool, request)
        .await
        .expect("insert approved request");
}

async fn try_insert_approved(pool: &PgPool, request: &ExecutionRequest) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO live_canary.execution_requests(
            id, opportunity_id, schema_version, chain_id, route_id,
            route_fingerprint, selected_size, token_path, origin_router,
            executor_address, executor_code_hash, calldata_hash,
            simulation_result_hash, plan_hash, pinned_block_number,
            pinned_block_hash, flash_asset, flash_amount, maximum_input_amount,
            minimum_profit, expected_profit, deadline, legs, gas_limit,
            max_fee_per_gas, max_priority_fee_per_gas, approved_by, approved_at,
            approval_deadline, policy_version, approval_digest, status
         )
         VALUES (
            $1, $2, $3, $4, $5, $6, $7::numeric, $8, $9, $10, $11, $12,
            $13, $14, $15::numeric, $16, $17, $18::numeric, $19::numeric,
            $20::numeric, $21::numeric, $22, $23, $24, $25::numeric,
            $26::numeric, $27, $28, $29, $30, $31, 'approved'
         )",
    )
    .bind(request.id)
    .bind(request.opportunity_id)
    .bind(&request.schema_version)
    .bind(i64::try_from(request.chain_id).expect("chain"))
    .bind(format!("0x{}", hex::encode(request.route_id)))
    .bind(&request.route_fingerprint)
    .bind(request.selected_size.to_string())
    .bind(
        serde_json::to_value(
            request
                .token_path
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
        )
        .expect("token path"),
    )
    .bind(request.origin_router.to_string())
    .bind(request.executor_address.to_string())
    .bind(&request.executor_code_hash)
    .bind(&request.calldata_hash)
    .bind(&request.simulation_result_hash)
    .bind(&request.plan_hash)
    .bind(request.pinned_block_number.to_string())
    .bind(&request.pinned_block_hash)
    .bind(request.flash_asset.to_string())
    .bind(request.flash_amount.to_string())
    .bind(request.maximum_input_amount.to_string())
    .bind(request.minimum_profit.to_string())
    .bind(request.expected_profit.to_string())
    .bind(request.deadline)
    .bind(serde_json::to_value(&request.legs).expect("legs"))
    .bind(i64::try_from(request.gas_limit).expect("gas"))
    .bind(request.max_fee_per_gas.to_string())
    .bind(request.max_priority_fee_per_gas.to_string())
    .bind(&request.approved_by)
    .bind(request.approved_at)
    .bind(request.approval_deadline)
    .bind(&request.policy_version)
    .bind(&request.approval_digest)
    .execute(pool)
    .await?;
    Ok(())
}
