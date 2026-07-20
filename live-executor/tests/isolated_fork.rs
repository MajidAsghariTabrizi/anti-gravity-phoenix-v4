use chrono::{Duration as ChronoDuration, Utc};
use phoenix_live_executor::abi::encode_execute_opportunity;
use phoenix_live_executor::model::{CanonicalAddress, ExecutionRequest, ValidatedLeg};
use phoenix_live_executor::rpc::{ExecutionRpc, HttpExecutionRpc};
use phoenix_live_executor::signer::{TransactionDraft, TransactionSigner};
use phoenix_live_executor::{ARBITRUM_ONE_CHAIN_ID, ARBITRUM_WETH_ADDRESS, REQUEST_SCHEMA_VERSION};
use url::Url;
use uuid::Uuid;
use zeroize::Zeroize;

#[tokio::test]
async fn signed_phoenix_executor_transaction_is_confined_to_isolated_anvil() {
    let Some(rpc_url) = std::env::var("PHOENIX_TEST_ISOLATED_FORK_RPC_URL").ok() else {
        eprintln!("isolated Anvil environment is unset; skipping fork integration");
        return;
    };
    let marker = std::env::var("PHOENIX_TEST_ISOLATED_FORK_CONFIRM")
        .expect("isolated fork confirmation is required");
    let mut private_key = std::env::var("PHOENIX_TEST_ISOLATED_FORK_SIGNER_KEY")
        .expect("isolated signer is required");
    let executor_address = CanonicalAddress::parse(
        &std::env::var("PHOENIX_TEST_EXECUTOR_ADDRESS")
            .expect("deployed PhoenixExecutor address is required")
            .to_ascii_lowercase(),
    )
    .expect("executor address");
    let signer_result = TransactionSigner::from_secret(&private_key, ARBITRUM_ONE_CHAIN_ID);
    private_key.zeroize();
    let signer = signer_result.expect("isolated signer");
    let rpc = HttpExecutionRpc::new_isolated_fork(
        Url::parse(&rpc_url).expect("isolated RPC URL"),
        &marker,
    )
    .expect("loopback-only transport");
    assert_eq!(
        rpc.chain_id().await.expect("chain id"),
        ARBITRUM_ONE_CHAIN_ID
    );

    let nonce = rpc
        .pending_nonce(signer.address())
        .await
        .expect("pending nonce");
    let request = deliberately_reverting_request(executor_address);
    let calldata =
        encode_execute_opportunity(&request, executor_address).expect("PhoenixExecutor calldata");
    let signed = signer
        .sign(TransactionDraft {
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            nonce,
            gas_limit: 1_000_000,
            max_fee_per_gas: 10_000_000_000,
            max_priority_fee_per_gas: 1_000_000_000,
            to: executor_address,
            calldata,
        })
        .expect("sign isolated transaction");
    let expected_hash = signed.tx_hash();
    let returned_hash = rpc
        .send_raw_transaction(signed.raw_bytes())
        .await
        .expect("submit only to isolated Anvil");
    assert_eq!(returned_hash, expected_hash);

    for _ in 0..50 {
        if let Some(receipt) = rpc
            .transaction_receipt(returned_hash)
            .await
            .expect("receipt query")
        {
            assert_eq!(receipt.transaction_hash, returned_hash);
            assert_eq!(
                receipt.status, 0,
                "unapproved fixture asset must make PhoenixExecutor revert"
            );
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("isolated PhoenixExecutor receipt was not observed");
}

fn deliberately_reverting_request(executor_address: CanonicalAddress) -> ExecutionRequest {
    let flash_asset = CanonicalAddress::parse(ARBITRUM_WETH_ADDRESS).expect("asset");
    let token_b =
        CanonicalAddress::parse("0x2222222222222222222222222222222222222222").expect("token");
    let now = Utc::now();
    let mut request = ExecutionRequest {
        id: Uuid::from_u128(31),
        opportunity_id: Uuid::from_u128(32),
        schema_version: REQUEST_SCHEMA_VERSION.to_string(),
        chain_id: ARBITRUM_ONE_CHAIN_ID,
        route_id: [33_u8; 32],
        origin_router: CanonicalAddress::parse("0x4444444444444444444444444444444444444444")
            .expect("router"),
        flash_asset,
        flash_amount: 1,
        maximum_input_amount: 1,
        minimum_profit: 1,
        expected_profit: 1,
        deadline: now + ChronoDuration::minutes(1),
        legs: vec![
            ValidatedLeg {
                pool: CanonicalAddress::parse("0x5555555555555555555555555555555555555555")
                    .expect("pool"),
                token_in: flash_asset,
                token_out: token_b,
                fee: 500,
                zero_for_one: true,
                min_amount_out: 1,
            },
            ValidatedLeg {
                pool: CanonicalAddress::parse("0x6666666666666666666666666666666666666666")
                    .expect("pool"),
                token_in: token_b,
                token_out: flash_asset,
                fee: 500,
                zero_for_one: false,
                min_amount_out: 1,
            },
        ],
        gas_limit: 1_000_000,
        max_fee_per_gas: 10_000_000_000,
        max_priority_fee_per_gas: 1_000_000_000,
        approved_by: "isolated-fork-fixture".to_string(),
        approved_at: now,
        policy_version: "isolated-fork-v1".to_string(),
        approval_digest: String::new(),
    };
    request.approval_digest = request
        .canonical_approval_digest()
        .expect("approval digest");
    assert_ne!(executor_address, flash_asset);
    request
}
