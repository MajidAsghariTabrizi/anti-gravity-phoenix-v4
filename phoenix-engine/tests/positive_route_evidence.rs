use ethabi::{ParamType, Token};
use phoenix_engine::positive_route_evidence::{
    analyze_stored_transaction, DiscoveryStatistics, StoredTransactionEvidence,
    TransactionProvenance, POSITIVE_ROUTE_EVIDENCE_NOT_FOUND, POSTGRES_FEED_EVENT_SOURCE,
};
use phoenix_engine::shadow_processor::RouteRegistry;
use primitive_types::{H160, U256};
use serde_json::json;

const LEGACY: &str = "0xe592427a0aece92de3edee1f18e0157c05861564";
const ROUTER_02: &str = "0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45";
const UNIVERSAL: &str = "0xa51afafe0263b40edaef0df8781ea9aa03e381a3";
const WETH: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
const USDC: &str = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";
const RECIPIENT: &str = "0x1111111111111111111111111111111111111111";

fn registry() -> RouteRegistry {
    RouteRegistry::from_json(include_str!(
        "../../fixtures/routes/weth_usdc_uniswap_v3.json"
    ))
    .unwrap()
}

fn stored(to: &str, calldata: String, hash_byte: char) -> StoredTransactionEvidence {
    StoredTransactionEvidence {
        provenance: TransactionProvenance {
            source: "test.synthetic".to_string(),
            feed_event_id: 1,
            recorded_at: "2026-01-01T00:00:00Z".to_string(),
            source_block_number: None,
            source_block_hash: None,
        },
        payload: json!({
            "schema_version": "phoenix.v4.normalized_tx.v1",
            "sequence": 7,
            "timestamp_unix_ms": 1_700_000_000_000_u64,
            "tx_hash": format!("0x{}", hash_byte.to_string().repeat(64)),
            "tx_type": "0x02",
            "chain_id": 42161,
            "from": RECIPIENT,
            "to": to,
            "nonce": 1,
            "value": "0",
            "calldata": calldata,
            "gas_limit": "300000",
            "max_fee_per_gas": "100000000",
            "max_priority_fee_per_gas": "1000000",
            "raw_tx": "AQID",
            "ingested_at_unix_ns": 1_700_000_000_123_456_789_i64
        }),
    }
}

fn abi_address(value: &str) -> Token {
    Token::Address(H160::from_slice(&hex::decode(&value[2..]).unwrap()))
}

fn selector(name: &str, parameters: &[ParamType]) -> [u8; 4] {
    ethabi::short_signature(name, parameters)
}

fn function_call(selector: [u8; 4], tokens: &[Token]) -> String {
    let mut bytes = selector.to_vec();
    bytes.extend(ethabi::encode(tokens));
    format!("0x{}", hex::encode(bytes))
}

fn legacy_single(token_in: &str, token_out: &str, fee: u32, exact_output: bool) -> String {
    let parameter = ParamType::Tuple(vec![
        ParamType::Address,
        ParamType::Address,
        ParamType::Uint(24),
        ParamType::Address,
        ParamType::Uint(256),
        ParamType::Uint(256),
        ParamType::Uint(256),
        ParamType::Uint(160),
    ]);
    let tuple = Token::Tuple(vec![
        abi_address(token_in),
        abi_address(token_out),
        Token::Uint(U256::from(fee)),
        abi_address(RECIPIENT),
        Token::Uint(U256::from(1_800_000_000_u64)),
        Token::Uint(U256::from(1_000_000_u64)),
        Token::Uint(U256::zero()),
        Token::Uint(U256::zero()),
    ]);
    function_call(
        selector(
            if exact_output {
                "exactOutputSingle"
            } else {
                "exactInputSingle"
            },
            std::slice::from_ref(&parameter),
        ),
        &[tuple],
    )
}

fn router02_single(token_in: &str, token_out: &str, fee: u32) -> String {
    let parameter = ParamType::Tuple(vec![
        ParamType::Address,
        ParamType::Address,
        ParamType::Uint(24),
        ParamType::Address,
        ParamType::Uint(256),
        ParamType::Uint(256),
        ParamType::Uint(160),
    ]);
    function_call(
        selector("exactInputSingle", std::slice::from_ref(&parameter)),
        &[Token::Tuple(vec![
            abi_address(token_in),
            abi_address(token_out),
            Token::Uint(U256::from(fee)),
            abi_address(RECIPIENT),
            Token::Uint(U256::from(1_000_000_u64)),
            Token::Uint(U256::zero()),
            Token::Uint(U256::zero()),
        ])],
    )
}

fn packed_path(token_in: &str, token_out: &str, fee: u32) -> Vec<u8> {
    let mut path = hex::decode(&token_in[2..]).unwrap();
    path.extend([(fee >> 16) as u8, (fee >> 8) as u8, fee as u8]);
    path.extend(hex::decode(&token_out[2..]).unwrap());
    path
}

fn universal_exact_input(token_in: &str, token_out: &str, fee: u32) -> String {
    let command_input = ethabi::encode(&[
        abi_address(RECIPIENT),
        Token::Uint(U256::from(1_000_000_u64)),
        Token::Uint(U256::zero()),
        Token::Bytes(packed_path(token_in, token_out, fee)),
        Token::Bool(true),
    ]);
    function_call(
        selector(
            "execute",
            &[
                ParamType::Bytes,
                ParamType::Array(Box::new(ParamType::Bytes)),
            ],
        ),
        &[
            Token::Bytes(vec![0x00]),
            Token::Array(vec![Token::Bytes(command_input)]),
        ],
    )
}

#[test]
fn all_reviewed_official_router_families_reach_the_configured_route() {
    let cases = [
        (
            LEGACY,
            legacy_single(WETH, USDC, 500, false),
            "legacy_swap_router",
        ),
        (ROUTER_02, router02_single(WETH, USDC, 500), "swap_router02"),
        (
            UNIVERSAL,
            universal_exact_input(WETH, USDC, 500),
            "universal_router",
        ),
    ];
    for (index, (router, calldata, expected_kind)) in cases.into_iter().enumerate() {
        let summary = analyze_stored_transaction(
            &stored(router, calldata, char::from(b'a' + index as u8)),
            &registry(),
        )
        .unwrap();
        assert_eq!(summary.router_kind.as_deref(), Some(expected_kind));
        assert!(summary.supported);
        assert_eq!(summary.exact_input, Some(true));
        assert_eq!(summary.recorded_at, "2026-01-01T00:00:00Z");
        assert_eq!(summary.input_amount.as_deref(), Some("1000000"));
        assert_eq!(summary.decoded_token_path, [WETH, USDC]);
        assert_eq!(summary.decoded_fee_path, [500]);
        assert_eq!(summary.candidate_count, 1);
        assert!(summary.candidate_produced);
        assert_eq!(
            summary.matched_route_ids,
            ["arbitrum-weth-usdc-uniswap-v3-500-3000"]
        );
        assert!(summary.shadow_only);
        assert!(!summary.execution_request_created);
        assert!(!summary.trusted_persisted_source);
        assert!(!summary.production_evidence);
    }
}

#[test]
fn exact_output_unknown_router_and_malformed_calldata_remain_fail_closed() {
    let exact_output = analyze_stored_transaction(
        &stored(LEGACY, legacy_single(WETH, USDC, 500, true), 'd'),
        &registry(),
    )
    .unwrap();
    assert!(exact_output.exact_output);
    assert!(!exact_output.supported);
    assert!(exact_output.input_amount.is_none());
    assert_eq!(exact_output.route_match_result, "unsupported_exact_output");

    let unknown = analyze_stored_transaction(
        &stored(
            "0x9999999999999999999999999999999999999999",
            router02_single(WETH, USDC, 500),
            'e',
        ),
        &registry(),
    )
    .unwrap();
    assert_eq!(unknown.route_match_result, "unsupported_router");
    assert_eq!(unknown.candidate_count, 0);

    let malformed = analyze_stored_transaction(
        &stored(LEGACY, "0x414bf38900".to_string(), 'f'),
        &registry(),
    )
    .unwrap();
    assert_eq!(malformed.route_match_result, "malformed_calldata");
    assert_eq!(malformed.candidate_count, 0);
}

#[test]
fn configured_pool_matching_is_direction_independent_and_rejects_unrelated_fees() {
    let forward = analyze_stored_transaction(
        &stored(ROUTER_02, router02_single(WETH, USDC, 500), '1'),
        &registry(),
    )
    .unwrap();
    let reverse = analyze_stored_transaction(
        &stored(ROUTER_02, router02_single(USDC, WETH, 500), '2'),
        &registry(),
    )
    .unwrap();
    assert_eq!(forward.decoded_pool_ids, reverse.decoded_pool_ids);
    assert_eq!(forward.affected_configured_pool_ids.len(), 1);

    let unrelated = analyze_stored_transaction(
        &stored(ROUTER_02, router02_single(WETH, USDC, 10_000), '3'),
        &registry(),
    )
    .unwrap();
    assert!(unrelated.supported);
    assert_eq!(unrelated.route_match_result, "decoded_but_irrelevant_pool");
    assert_eq!(unrelated.candidate_count, 0);
}

#[test]
fn synthetic_candidate_tests_cannot_claim_real_positive_evidence() {
    let summary = analyze_stored_transaction(
        &stored(UNIVERSAL, universal_exact_input(WETH, USDC, 3000), '4'),
        &registry(),
    )
    .unwrap();
    assert!(summary.candidate_produced);
    assert!(!summary.production_evidence);

    let mut statistics = DiscoveryStatistics::default();
    statistics.observe(&summary);
    assert_eq!(statistics.candidate_count, 1);
    assert_eq!(statistics.production_candidate_count, 0);
    assert_eq!(
        statistics.terminal_result(),
        POSITIVE_ROUTE_EVIDENCE_NOT_FOUND
    );
}

#[test]
fn trusted_postgres_history_remains_distinct_from_configured_route_evidence() {
    let mut evidence = stored(ROUTER_02, router02_single(WETH, USDC, 10_000), '5');
    evidence.provenance.source = POSTGRES_FEED_EVENT_SOURCE.to_string();
    let summary = analyze_stored_transaction(&evidence, &registry()).unwrap();

    assert!(summary.supported);
    assert!(summary.trusted_persisted_source);
    assert!(!summary.candidate_produced);
    assert!(!summary.production_evidence);
    assert_eq!(summary.route_match_result, "decoded_but_irrelevant_pool");
}
