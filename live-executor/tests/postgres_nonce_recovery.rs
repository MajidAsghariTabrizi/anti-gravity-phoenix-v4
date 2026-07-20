use chrono::{Duration as ChronoDuration, Utc};
use phoenix_live_executor::config::{ExecutorConfig, SafetyLimits};
use phoenix_live_executor::model::{
    AttemptStatus, CanonicalAddress, ExecutionRequest, ReceiptOutcome, Settlement, TransactionHash,
    ValidatedLeg,
};
use phoenix_live_executor::signer::TransactionSigner;
use phoenix_live_executor::store::{ExecutorStore, PostgresExecutorStore};
use phoenix_live_executor::{ARBITRUM_ONE_CHAIN_ID, ARBITRUM_WETH_ADDRESS, REQUEST_SCHEMA_VERSION};
use sqlx::{PgPool, Row};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

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

    sqlx::raw_sql("DROP SCHEMA live_canary CASCADE")
        .execute(&pool)
        .await
        .expect("drop service schema");
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
    let token_b =
        CanonicalAddress::parse("0x2222222222222222222222222222222222222222").expect("token");
    let mut request = ExecutionRequest {
        id,
        opportunity_id: Uuid::from_u128(id.as_u128() + 100),
        schema_version: REQUEST_SCHEMA_VERSION.to_string(),
        chain_id: ARBITRUM_ONE_CHAIN_ID,
        route_id: [17_u8; 32],
        origin_router: CanonicalAddress::parse("0x4444444444444444444444444444444444444444")
            .expect("router"),
        flash_asset,
        flash_amount: 1_000,
        maximum_input_amount: 1_000,
        minimum_profit: 100,
        expected_profit: 500,
        deadline: now + ChronoDuration::minutes(2),
        legs: vec![
            ValidatedLeg {
                pool: CanonicalAddress::parse("0x5555555555555555555555555555555555555555")
                    .expect("pool"),
                token_in: flash_asset,
                token_out: token_b,
                fee: 500,
                zero_for_one: true,
                min_amount_out: 900,
            },
            ValidatedLeg {
                pool: CanonicalAddress::parse("0x6666666666666666666666666666666666666666")
                    .expect("pool"),
                token_in: token_b,
                token_out: flash_asset,
                fee: 500,
                zero_for_one: false,
                min_amount_out: 1_100,
            },
        ],
        gas_limit: 400_000,
        max_fee_per_gas: 900,
        max_priority_fee_per_gas: 90,
        approved_by: "isolated-postgres-test".to_string(),
        approved_at: now - ChronoDuration::seconds(1),
        policy_version: "live-canary-v1".to_string(),
        approval_digest: String::new(),
    };
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
            id, opportunity_id, schema_version, chain_id, route_id, origin_router,
            flash_asset, flash_amount, maximum_input_amount, minimum_profit,
            expected_profit, deadline, legs, gas_limit, max_fee_per_gas,
            max_priority_fee_per_gas, approved_by, approved_at, policy_version,
            approval_digest, status
         )
         VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8::numeric, $9::numeric, $10::numeric,
            $11::numeric, $12, $13, $14, $15::numeric, $16::numeric, $17, $18,
            $19, $20, 'approved'
         )",
    )
    .bind(request.id)
    .bind(request.opportunity_id)
    .bind(&request.schema_version)
    .bind(i64::try_from(request.chain_id).expect("chain"))
    .bind(format!("0x{}", hex::encode(request.route_id)))
    .bind(request.origin_router.to_string())
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
    .bind(&request.policy_version)
    .bind(&request.approval_digest)
    .execute(pool)
    .await?;
    Ok(())
}
