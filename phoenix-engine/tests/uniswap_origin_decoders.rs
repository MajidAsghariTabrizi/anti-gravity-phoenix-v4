use ethabi::ethereum_types::{H160, U256};
use ethabi::{ParamType, Token};
use phoenix_engine::domain::{Address, ChainId, SequenceNumber, TxHash};
use phoenix_engine::messaging::NormalizedTx;
use phoenix_engine::origin::{
    DecodedSwapKind, OriginClassification, OriginDetector, OuterSelectorKind, RouterKind,
    UnsupportedReason, WrapperKind, LEGACY_SWAP_ROUTER_ADDRESS, SWAP_ROUTER_02_ADDRESS,
    UNIVERSAL_ROUTER_ADDRESS,
};
use phoenix_engine::shadow_processor::RouteRegistry;

const WETH: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
const USDC: &str = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";
const DAI: &str = "0xda10009cbd5d07dd0cecc66161fc93d7c9000da1";
const RECIPIENT: &str = "0x1111111111111111111111111111111111111111";
const UNVERIFIED_ROUTER: &str = "0x1b81d678ffb9c0263b24a97847620c99d213eb14";

fn tuple(types: Vec<ParamType>) -> ParamType {
    ParamType::Tuple(types)
}

fn legacy_exact_input_single_type() -> ParamType {
    tuple(vec![
        ParamType::Address,
        ParamType::Address,
        ParamType::Uint(24),
        ParamType::Address,
        ParamType::Uint(256),
        ParamType::Uint(256),
        ParamType::Uint(256),
        ParamType::Uint(160),
    ])
}

fn legacy_exact_input_type() -> ParamType {
    tuple(vec![
        ParamType::Bytes,
        ParamType::Address,
        ParamType::Uint(256),
        ParamType::Uint(256),
        ParamType::Uint(256),
    ])
}

fn router02_exact_input_single_type() -> ParamType {
    tuple(vec![
        ParamType::Address,
        ParamType::Address,
        ParamType::Uint(24),
        ParamType::Address,
        ParamType::Uint(256),
        ParamType::Uint(256),
        ParamType::Uint(160),
    ])
}

fn selector(name: &str, types: &[ParamType]) -> [u8; 4] {
    ethabi::short_signature(name, types)
}

fn abi_address(value: &str) -> Token {
    let bytes = hex::decode(value.trim_start_matches("0x")).unwrap();
    Token::Address(H160::from_slice(&bytes))
}

fn function_call(selector: [u8; 4], tokens: &[Token]) -> Vec<u8> {
    let mut bytes = selector.to_vec();
    bytes.extend(ethabi::encode(tokens));
    bytes
}

fn calldata(bytes: Vec<u8>) -> String {
    format!("0x{}", hex::encode(bytes))
}

fn legacy_single(deadline: u128, amount_in: u128, token_in: &str, token_out: &str) -> Vec<u8> {
    function_call(
        selector("exactInputSingle", &[legacy_exact_input_single_type()]),
        &[Token::Tuple(vec![
            abi_address(token_in),
            abi_address(token_out),
            Token::Uint(U256::from(500_u64)),
            abi_address(RECIPIENT),
            Token::Uint(U256::from(deadline)),
            Token::Uint(U256::from(amount_in)),
            Token::Uint(U256::from(1_u64)),
            Token::Uint(U256::zero()),
        ])],
    )
}

fn router02_single(amount_in: u128, token_in: &str, token_out: &str) -> Vec<u8> {
    function_call(
        selector("exactInputSingle", &[router02_exact_input_single_type()]),
        &[Token::Tuple(vec![
            abi_address(token_in),
            abi_address(token_out),
            Token::Uint(U256::from(500_u64)),
            abi_address(RECIPIENT),
            Token::Uint(U256::from(amount_in)),
            Token::Uint(U256::from(1_u64)),
            Token::Uint(U256::zero()),
        ])],
    )
}

fn packed_path(tokens: &[&str], fees: &[u32]) -> Vec<u8> {
    assert_eq!(tokens.len(), fees.len() + 1);
    let mut path = hex::decode(tokens[0].trim_start_matches("0x")).unwrap();
    for (fee, token) in fees.iter().zip(tokens.iter().skip(1)) {
        path.extend(fee.to_be_bytes()[1..4].iter());
        path.extend(hex::decode(token.trim_start_matches("0x")).unwrap());
    }
    path
}

fn legacy_exact_input(path: Vec<u8>, amount_in: u128) -> Vec<u8> {
    function_call(
        selector("exactInput", &[legacy_exact_input_type()]),
        &[Token::Tuple(vec![
            Token::Bytes(path),
            abi_address(RECIPIENT),
            Token::Uint(U256::from(1_800_000_000_u64)),
            Token::Uint(U256::from(amount_in)),
            Token::Uint(U256::from(1_u64)),
        ])],
    )
}

fn legacy_multicall(calls: Vec<Vec<u8>>) -> Vec<u8> {
    function_call(
        selector("multicall", &[ParamType::Array(Box::new(ParamType::Bytes))]),
        &[Token::Array(calls.into_iter().map(Token::Bytes).collect())],
    )
}

fn universal_v3_exact_in(path: Vec<u8>, amount_in: u128) -> Vec<u8> {
    ethabi::encode(&[
        abi_address(RECIPIENT),
        Token::Uint(U256::from(amount_in)),
        Token::Uint(U256::from(1_u64)),
        Token::Bytes(path),
        Token::Bool(true),
    ])
}

fn universal_execute(commands: Vec<u8>, inputs: Vec<Vec<u8>>, deadline: Option<u128>) -> Vec<u8> {
    let mut types = vec![
        ParamType::Bytes,
        ParamType::Array(Box::new(ParamType::Bytes)),
    ];
    let mut tokens = vec![
        Token::Bytes(commands),
        Token::Array(inputs.into_iter().map(Token::Bytes).collect()),
    ];
    if let Some(deadline) = deadline {
        types.push(ParamType::Uint(256));
        tokens.push(Token::Uint(U256::from(deadline)));
    }
    function_call(selector("execute", &types), &tokens)
}

fn detector() -> OriginDetector {
    OriginDetector::new(
        [
            LEGACY_SWAP_ROUTER_ADDRESS,
            SWAP_ROUTER_02_ADDRESS,
            UNIVERSAL_ROUTER_ADDRESS,
        ]
        .into_iter()
        .map(|value| Address::parse(value).unwrap())
        .collect(),
    )
    .unwrap()
}

fn transaction(to: &str, calldata: String) -> NormalizedTx {
    NormalizedTx {
        sequence: SequenceNumber(7),
        tx_hash: TxHash(format!("0x{}", "a".repeat(64))),
        tx_type: "0x02".to_string(),
        chain_id: ChainId(42161),
        from: Address::parse(RECIPIENT).unwrap(),
        to: Some(Address::parse(to).unwrap()),
        nonce: 1,
        value: "0".to_string(),
        calldata,
        gas_limit: "300000".to_string(),
        max_fee_per_gas: "100".to_string(),
        max_priority_fee_per_gas: "1".to_string(),
    }
}

fn classify(to: &str, bytes: Vec<u8>) -> OriginClassification {
    detector().classify(&transaction(to, calldata(bytes)))
}

fn supported(classification: OriginClassification) -> phoenix_engine::origin::OriginEvent {
    match classification {
        OriginClassification::SupportedSwapOrigin(event) => event,
        other => panic!("expected supported origin, got {other:?}"),
    }
}

fn unsupported(classification: OriginClassification) -> phoenix_engine::origin::OriginEvidence {
    match classification {
        OriginClassification::KnownRouterUnsupportedCommand(evidence) => evidence,
        other => panic!("expected unsupported origin, got {other:?}"),
    }
}

fn malformed(classification: OriginClassification) -> phoenix_engine::origin::OriginEvidence {
    match classification {
        OriginClassification::Malformed(evidence) => evidence,
        other => panic!("expected malformed origin, got {other:?}"),
    }
}

#[test]
fn selectors_are_derived_from_pinned_official_abis() {
    assert_eq!(
        hex::encode(selector(
            "exactInputSingle",
            &[legacy_exact_input_single_type()]
        )),
        "414bf389"
    );
    assert_eq!(
        hex::encode(selector("exactInput", &[legacy_exact_input_type()])),
        "c04b8d59"
    );
    assert_eq!(
        hex::encode(selector(
            "exactOutputSingle",
            &[legacy_exact_input_single_type()]
        )),
        "db3e2198"
    );
    assert_eq!(
        hex::encode(selector(
            "multicall",
            &[ParamType::Array(Box::new(ParamType::Bytes))]
        )),
        "ac9650d8"
    );
    assert_eq!(
        hex::encode(selector(
            "exactInputSingle",
            &[router02_exact_input_single_type()]
        )),
        "04e45aaf"
    );
    assert_eq!(
        hex::encode(selector(
            "execute",
            &[
                ParamType::Bytes,
                ParamType::Array(Box::new(ParamType::Bytes))
            ]
        )),
        "24856bc3"
    );
    assert_eq!(
        hex::encode(selector(
            "execute",
            &[
                ParamType::Bytes,
                ParamType::Array(Box::new(ParamType::Bytes)),
                ParamType::Uint(256)
            ]
        )),
        "3593564c"
    );
}

#[test]
fn router_specific_single_hop_layouts_read_the_correct_amount_slot() {
    let legacy = supported(classify(
        LEGACY_SWAP_ROUTER_ADDRESS,
        legacy_single(111, 222, WETH, USDC),
    ));
    assert_eq!(legacy.amount.0, 222);
    assert_eq!(
        legacy.classification_evidence.router_kind,
        Some(RouterKind::LegacySwapRouter)
    );
    assert_eq!(
        legacy.classification_evidence.outer_selector_kind,
        OuterSelectorKind::LegacyExactInputSingle
    );

    let router02 = supported(classify(
        SWAP_ROUTER_02_ADDRESS,
        router02_single(333, WETH, USDC),
    ));
    assert_eq!(router02.amount.0, 333);
    assert_eq!(
        router02.classification_evidence.router_kind,
        Some(RouterKind::SwapRouter02)
    );
}

#[test]
fn legacy_and_router02_selectors_are_never_decoded_with_the_other_layout() {
    let legacy_on_router02 = unsupported(classify(
        SWAP_ROUTER_02_ADDRESS,
        legacy_single(111, 222, WETH, USDC),
    ));
    let router02_on_legacy = unsupported(classify(
        LEGACY_SWAP_ROUTER_ADDRESS,
        router02_single(333, WETH, USDC),
    ));
    assert_eq!(
        legacy_on_router02.unsupported_reason,
        UnsupportedReason::UnknownSelector
    );
    assert_eq!(
        router02_on_legacy.unsupported_reason,
        UnsupportedReason::UnknownSelector
    );
}

#[test]
fn canonical_pool_identity_is_independent_of_swap_direction() {
    let forward = supported(classify(
        SWAP_ROUTER_02_ADDRESS,
        router02_single(100, WETH, USDC),
    ));
    let reverse = supported(classify(
        SWAP_ROUTER_02_ADDRESS,
        router02_single(100, USDC, WETH),
    ));
    assert_eq!(
        forward.candidate_touched_pools,
        reverse.candidate_touched_pools
    );
    assert_eq!(
        forward.candidate_touched_pools[0].0,
        format!("{WETH}:{USDC}:500")
    );
    assert_ne!(forward.swap_path, reverse.swap_path);
}

#[test]
fn legacy_exact_input_decodes_one_and_multiple_hops() {
    let one_hop = supported(classify(
        LEGACY_SWAP_ROUTER_ADDRESS,
        legacy_exact_input(packed_path(&[WETH, USDC], &[500]), 101),
    ));
    assert_eq!(one_hop.swap_path.len(), 2);
    assert_eq!(one_hop.candidate_touched_pools.len(), 1);

    let two_hop = supported(classify(
        LEGACY_SWAP_ROUTER_ADDRESS,
        legacy_exact_input(packed_path(&[WETH, USDC, DAI], &[500, 3000]), 202),
    ));
    assert_eq!(two_hop.swap_path.len(), 3);
    assert_eq!(two_hop.candidate_touched_pools.len(), 2);
    assert_eq!(two_hop.classification_evidence.v3_hop_count, 2);
    assert_eq!(
        two_hop.classification_evidence.decoded_swap_kind,
        DecodedSwapKind::V3ExactInput
    );
}

#[test]
fn malformed_truncated_trailing_and_oversized_v3_paths_fail_closed() {
    for path in [vec![0_u8; 42], vec![0_u8; 44], vec![1_u8; 20 + 23 * 9]] {
        let evidence = malformed(classify(
            LEGACY_SWAP_ROUTER_ADDRESS,
            legacy_exact_input(path, 100),
        ));
        assert_eq!(
            evidence.unsupported_reason,
            UnsupportedReason::MalformedCalldata
        );
    }
}

#[test]
fn legacy_multicall_allows_one_swap_and_reviewed_companions() {
    let refund = function_call(selector("refundETH", &[]), &[]);
    let event = supported(classify(
        LEGACY_SWAP_ROUTER_ADDRESS,
        legacy_multicall(vec![legacy_single(1, 500, WETH, USDC), refund]),
    ));
    assert_eq!(
        event.classification_evidence.wrapper_kind,
        WrapperKind::Multicall
    );
    assert_eq!(event.classification_evidence.command_count, 2);
    assert_eq!(
        event.decoded_commands,
        vec!["multicall", "exactInputSingle", "refundETH"]
    );
}

#[test]
fn malformed_reviewed_multicall_companion_is_not_classified_as_unknown() {
    let mut malformed_refund = selector("refundETH", &[]).to_vec();
    malformed_refund.extend([0_u8; 32]);
    let evidence = malformed(classify(
        LEGACY_SWAP_ROUTER_ADDRESS,
        legacy_multicall(vec![legacy_single(1, 500, WETH, USDC), malformed_refund]),
    ));

    assert_eq!(
        evidence.unsupported_reason,
        UnsupportedReason::MalformedCalldata
    );
}

#[test]
fn multiple_swap_multicall_is_explicitly_ambiguous() {
    let evidence = unsupported(classify(
        LEGACY_SWAP_ROUTER_ADDRESS,
        legacy_multicall(vec![
            legacy_single(1, 100, WETH, USDC),
            legacy_exact_input(packed_path(&[WETH, USDC], &[500]), 100),
        ]),
    ));
    assert_eq!(
        evidence.unsupported_reason,
        UnsupportedReason::AmbiguousMultiSwap
    );
}

#[test]
fn malformed_and_overlapping_multicall_offsets_fail_closed() {
    let valid = legacy_multicall(vec![
        legacy_single(1, 100, WETH, USDC),
        function_call(selector("refundETH", &[]), &[]),
    ]);
    let mut overlapping = valid.clone();
    let arguments = &mut overlapping[4..];
    let first_offset = arguments[64..96].to_vec();
    arguments[96..128].copy_from_slice(&first_offset);
    assert_eq!(
        malformed(classify(LEGACY_SWAP_ROUTER_ADDRESS, overlapping)).unsupported_reason,
        UnsupportedReason::MalformedCalldata
    );

    let mut truncated = valid;
    truncated.pop();
    assert_eq!(
        malformed(classify(LEGACY_SWAP_ROUTER_ADDRESS, truncated)).unsupported_reason,
        UnsupportedReason::MalformedCalldata
    );
}

#[test]
fn oversized_and_recursive_multicalls_fail_closed() {
    let refund = function_call(selector("refundETH", &[]), &[]);
    let oversized = legacy_multicall(vec![refund; 17]);
    assert_eq!(
        malformed(classify(LEGACY_SWAP_ROUTER_ADDRESS, oversized)).unsupported_reason,
        UnsupportedReason::MalformedCalldata
    );

    let nested = legacy_multicall(vec![legacy_single(1, 100, WETH, USDC)]);
    let evidence = unsupported(classify(
        LEGACY_SWAP_ROUTER_ADDRESS,
        legacy_multicall(vec![nested]),
    ));
    assert_eq!(
        evidence.unsupported_reason,
        UnsupportedReason::NestedSubPlan
    );
}

#[test]
fn universal_router_execute_overloads_decode_one_v3_exact_in() {
    for deadline in [None, Some(1_800_000_000)] {
        let input = universal_v3_exact_in(packed_path(&[WETH, USDC], &[500]), 777);
        let event = supported(classify(
            UNIVERSAL_ROUTER_ADDRESS,
            universal_execute(vec![0x00], vec![input], deadline),
        ));
        assert_eq!(event.amount.0, 777);
        assert_eq!(
            event.classification_evidence.wrapper_kind,
            WrapperKind::UniversalRouter
        );
        assert_eq!(
            event.classification_evidence.outer_selector_kind,
            if deadline.is_some() {
                OuterSelectorKind::UniversalExecuteWithDeadline
            } else {
                OuterSelectorKind::UniversalExecute
            }
        );
    }
}

#[test]
fn universal_router_reviewed_payment_companion_does_not_change_route() {
    let sweep = ethabi::encode(&[
        abi_address(WETH),
        abi_address(RECIPIENT),
        Token::Uint(U256::from(1_u64)),
    ]);
    let swap = universal_v3_exact_in(packed_path(&[WETH, USDC], &[500]), 555);
    let event = supported(classify(
        UNIVERSAL_ROUTER_ADDRESS,
        universal_execute(vec![0x04, 0x00], vec![sweep, swap], None),
    ));
    assert_eq!(event.amount.0, 555);
    assert_eq!(event.classification_evidence.command_count, 2);
    assert_eq!(
        event.candidate_touched_pools[0].0,
        format!("{WETH}:{USDC}:500")
    );
}

#[test]
fn universal_router_exact_output_multiple_swaps_and_unknown_commands_are_rejected() {
    let swap = universal_v3_exact_in(packed_path(&[WETH, USDC], &[500]), 100);
    assert_eq!(
        unsupported(classify(
            UNIVERSAL_ROUTER_ADDRESS,
            universal_execute(vec![0x01], vec![swap.clone()], None),
        ))
        .unsupported_reason,
        UnsupportedReason::ExactOutput
    );
    assert_eq!(
        unsupported(classify(
            UNIVERSAL_ROUTER_ADDRESS,
            universal_execute(vec![0x00, 0x00], vec![swap.clone(), swap.clone()], None),
        ))
        .unsupported_reason,
        UnsupportedReason::AmbiguousMultiSwap
    );
    assert_eq!(
        unsupported(classify(
            UNIVERSAL_ROUTER_ADDRESS,
            universal_execute(vec![0x20], vec![Vec::new()], None),
        ))
        .unsupported_reason,
        UnsupportedReason::UnknownCommand
    );
    assert_eq!(
        unsupported(classify(
            UNIVERSAL_ROUTER_ADDRESS,
            universal_execute(vec![0x80], vec![swap], None),
        ))
        .unsupported_reason,
        UnsupportedReason::OptionalSwap
    );
}

#[test]
fn universal_router_length_mismatch_nested_plan_and_malformed_input_fail_closed() {
    let swap = universal_v3_exact_in(packed_path(&[WETH, USDC], &[500]), 100);
    assert_eq!(
        malformed(classify(
            UNIVERSAL_ROUTER_ADDRESS,
            universal_execute(vec![0x00], Vec::new(), None),
        ))
        .unsupported_reason,
        UnsupportedReason::MalformedCalldata
    );
    assert_eq!(
        unsupported(classify(
            UNIVERSAL_ROUTER_ADDRESS,
            universal_execute(vec![0x21], vec![Vec::new()], None),
        ))
        .unsupported_reason,
        UnsupportedReason::NestedSubPlan
    );
    assert_eq!(
        malformed(classify(
            UNIVERSAL_ROUTER_ADDRESS,
            universal_execute(vec![0x00], vec![swap[..swap.len() - 1].to_vec()], None),
        ))
        .unsupported_reason,
        UnsupportedReason::MalformedCalldata
    );
}

#[test]
fn exact_output_is_never_represented_as_exact_input() {
    let selector = selector("exactOutputSingle", &[legacy_exact_input_single_type()]);
    let evidence = unsupported(classify(
        LEGACY_SWAP_ROUTER_ADDRESS,
        function_call(selector, &[]),
    ));
    assert_eq!(evidence.unsupported_reason, UnsupportedReason::ExactOutput);
    assert_eq!(evidence.exact_in, Some(false));
    assert!(!evidence.supported);
}

#[test]
fn unknown_destinations_including_observed_third_party_remain_possible_aggregators() {
    let bytes = legacy_single(1, 100, WETH, USDC);
    for destination in [
        UNVERIFIED_ROUTER,
        "0x9999999999999999999999999999999999999999",
    ] {
        assert_eq!(
            detector().classify(&transaction(destination, calldata(bytes.clone()))),
            OriginClassification::PossibleAggregator
        );
    }
    assert!(OriginDetector::new(vec![Address::parse(UNVERIFIED_ROUTER).unwrap()]).is_err());
}

#[test]
fn supported_official_pool_identity_reaches_existing_route_registry() {
    let event = supported(classify(
        SWAP_ROUTER_02_ADDRESS,
        router02_single(100, USDC, WETH),
    ));
    let registry = RouteRegistry::from_json(&route_registry_json()).unwrap();
    assert_eq!(
        registry
            .affected_routes(&event.candidate_touched_pools)
            .len(),
        1
    );
}

#[test]
fn classification_evidence_is_bounded_and_excludes_raw_identity_material() {
    let event = supported(classify(
        UNIVERSAL_ROUTER_ADDRESS,
        universal_execute(
            vec![0x00],
            vec![universal_v3_exact_in(
                packed_path(&[WETH, USDC], &[500]),
                100,
            )],
            None,
        ),
    ));
    let evidence = serde_json::to_string(&event.classification_evidence).unwrap();
    for required in [
        "router_kind",
        "outer_selector_kind",
        "wrapper_kind",
        "decoded_swap_kind",
        "command_count",
        "v3_hop_count",
        "exact_in",
        "supported",
        "unsupported_reason",
    ] {
        assert!(evidence.contains(required));
    }
    for forbidden in ["0x", "tx_hash", "calldata", WETH, USDC] {
        assert!(!evidence.contains(forbidden));
    }
}

#[test]
fn decoder_source_has_no_rpc_or_payload_logging_path() {
    let source = include_str!("../src/origin/uniswap.rs");
    for forbidden in [
        "reqwest",
        "rpc_gateway",
        "eth_call",
        "tracing::",
        "println!",
        "dbg!",
    ] {
        assert!(!source.contains(forbidden));
    }
}

#[test]
fn oversized_outer_calldata_and_zero_amount_fail_closed() {
    let oversized = format!("0x{}", "00".repeat(256 * 1024 + 1));
    let evidence = match detector().classify(&transaction(LEGACY_SWAP_ROUTER_ADDRESS, oversized)) {
        OriginClassification::Malformed(evidence) => evidence,
        other => panic!("expected malformed origin, got {other:?}"),
    };
    assert_eq!(
        evidence.unsupported_reason,
        UnsupportedReason::MalformedCalldata
    );
    assert_eq!(
        malformed(classify(
            SWAP_ROUTER_02_ADDRESS,
            router02_single(0, WETH, USDC),
        ))
        .unsupported_reason,
        UnsupportedReason::MalformedCalldata
    );
}

fn route_registry_json() -> String {
    format!(
        r#"[{{
            "route_id":"weth-usdc-two-pool",
            "route_fingerprint":"weth-usdc-two-pool-v1",
            "trigger_pool_id":"{WETH}:{USDC}:500",
            "legs":[
                {{"pool_id":"{WETH}:{USDC}:500","state_target":"0x0000000000000000000000000000000000001001","protocol":"UniswapV3","fee":500,"token_in":"{WETH}","token_out":"{USDC}","direction":"zero_for_one"}},
                {{"pool_id":"comparison-pool","state_target":"0x0000000000000000000000000000000000002001","protocol":"SushiSwapV3","fee":500,"token_in":"{USDC}","token_out":"{WETH}","direction":"one_for_zero"}}
            ],
            "strategy":{{
                "min_input_amount":"100","max_input_amount":"1000","max_evaluations":16,
                "minimum_net_profit":"1","flash_premium_bps":5,"minimum_slippage_bps":10,
                "protocol_fees":"0","estimated_execution_gas":500000,"l1_data_fee":"1",
                "contract_overhead":"1","failed_attempt_gas_cost":"1","failure_probability_bps":500,
                "stale_state_loss":"1","stale_quote_probability_bps":100,"state_drift_reserve":"1",
                "latency_reserve":"1","uncertainty_reserve":"1","replacement_transaction_cost":"1",
                "probability_of_success_bps":8000,"max_gas_price_wei":"1000000000000",
                "max_quote_age_ms":2000,"max_simulation_age_ms":2000,"min_confidence_bps":9000
            }}
        }}]"#
    )
}
